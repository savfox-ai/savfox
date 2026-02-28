//! Runtime configuration validation with type checking, range validation, and cross-field
//! validation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A validation error with field path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// Validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn ok() -> Self {
        Self {
            valid: true,
            errors: vec![],
        }
    }

    pub fn with_errors(errors: Vec<ValidationError>) -> Self {
        let valid = !errors.iter().any(|e| e.severity == Severity::Error);
        Self { valid, errors }
    }
}

/// Validate a configuration value
pub fn validate_config(config: &Value) -> ValidationResult {
    let mut errors = Vec::new();

    if let Value::Object(map) = config {
        // Validate gateway section
        if let Some(gateway) = map.get("gateway") {
            validate_gateway_section(gateway, &mut errors);
        }

        // Validate agents section
        if let Some(agents) = map.get("agents") {
            validate_agents_section(agents, &mut errors);
        }

        // Validate models section
        if let Some(models) = map.get("models") {
            validate_models_section(models, &mut errors);
        }
    }

    ValidationResult::with_errors(errors)
}

fn validate_gateway_section(gateway: &Value, errors: &mut Vec<ValidationError>) {
    if let Value::Object(map) = gateway {
        // Port validation
        if let Some(port) = map.get("port") {
            if let Some(p) = port.as_u64() {
                if p == 0 || p > 65535 {
                    errors.push(ValidationError {
                        field: "gateway.port".to_string(),
                        message: format!("Port must be between 1 and 65535, got {p}"),
                        severity: Severity::Error,
                    });
                }
            } else if !port.is_null() {
                errors.push(ValidationError {
                    field: "gateway.port".to_string(),
                    message: "Port must be a number".to_string(),
                    severity: Severity::Error,
                });
            }
        }

        // Host validation
        if let Some(host) = map.get("host") {
            if let Some(h) = host.as_str() {
                if h.is_empty() {
                    errors.push(ValidationError {
                        field: "gateway.host".to_string(),
                        message: "Host cannot be empty".to_string(),
                        severity: Severity::Error,
                    });
                }
            }
        }

        // Token validation
        if let Some(token) = map.get("token") {
            if let Some(t) = token.as_str() {
                if t.len() < 8 {
                    errors.push(ValidationError {
                        field: "gateway.token".to_string(),
                        message: "Token should be at least 8 characters for security".to_string(),
                        severity: Severity::Warning,
                    });
                }
            }
        }
    }
}

fn validate_agents_section(agents: &Value, errors: &mut Vec<ValidationError>) {
    if let Value::Object(map) = agents {
        for (name, agent) in map {
            if let Value::Object(agent_map) = agent {
                // Validate model reference
                if let Some(models) = agent_map.get("models") {
                    if let Value::Object(m) = models {
                        if let Some(primary) = m.get("primary") {
                            if let Some(p) = primary.as_str() {
                                if !p.contains('/') {
                                    errors.push(ValidationError {
                                        field: format!("agents.{name}.models.primary"),
                                        message: format!(
                                            "Model ID should be in 'provider/model' format, got '{p}'"
                                        ),
                                        severity: Severity::Warning,
                                    });
                                }
                            }
                        }
                    }
                }

                // Validate temperature
                if let Some(temp) = agent_map.get("temperature") {
                    if let Some(t) = temp.as_f64() {
                        if !(0.0..=2.0).contains(&t) {
                            errors.push(ValidationError {
                                field: format!("agents.{name}.temperature"),
                                message: format!(
                                    "Temperature must be between 0.0 and 2.0, got {t}"
                                ),
                                severity: Severity::Error,
                            });
                        }
                    }
                }

                validate_agent_workspace(name, agent_map, errors);
            }
        }
    }
}

fn validate_agent_workspace(
    agent_name: &str,
    agent_map: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    let workspace_keys = ["workspace_dir", "workspace", "workdir"];
    let Some((workspace_key, workspace_raw)) = workspace_keys.iter().find_map(|key| {
        agent_map
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|value| (*key, value.to_string()))
    }) else {
        return;
    };

    let field = format!("agents.{agent_name}.{workspace_key}");
    let workspace_path = PathBuf::from(&workspace_raw);
    let auto_create = agent_map
        .get("workspace_auto_create")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let auto_create_confirmed = agent_map
        .get("workspace_auto_create_confirmed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !workspace_path.exists() {
        if auto_create && auto_create_confirmed {
            if let Err(err) = std::fs::create_dir_all(&workspace_path) {
                errors.push(ValidationError {
                    field: field.clone(),
                    message: format!(
                        "Workspace path does not exist and auto-create failed: {} ({err})",
                        workspace_path.display()
                    ),
                    severity: Severity::Error,
                });
                return;
            }
        } else {
            let hint = if auto_create && !auto_create_confirmed {
                "set workspace_auto_create_confirmed=true to confirm auto-create"
            } else {
                "create the directory or enable workspace_auto_create with confirmation"
            };
            errors.push(ValidationError {
                field: field.clone(),
                message: format!(
                    "Workspace path does not exist: {} ({hint})",
                    workspace_path.display()
                ),
                severity: Severity::Error,
            });
            return;
        }
    }

    if !workspace_path.is_dir() {
        errors.push(ValidationError {
            field: field.clone(),
            message: format!(
                "Workspace path exists but is not a directory: {}",
                workspace_path.display()
            ),
            severity: Severity::Error,
        });
        return;
    }

    if !is_workspace_path_writable(&workspace_path) {
        errors.push(ValidationError {
            field: field.clone(),
            message: format!(
                "Workspace path is not writable: {}",
                workspace_path.display()
            ),
            severity: Severity::Error,
        });
        return;
    }

    if !is_safe_workspace_path(&workspace_path) {
        errors.push(ValidationError {
            field,
            message: format!(
                "Workspace path is outside safe roots (current project or ~/.savfox): {}",
                workspace_path.display()
            ),
            severity: Severity::Warning,
        });
    }
}

fn is_workspace_path_writable(path: &Path) -> bool {
    let probe = path.join(format!(".savfox-write-check-{}", uuid::Uuid::now_v7()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn is_safe_workspace_path(path: &Path) -> bool {
    let abs = absolute_path(path);
    let mut safe_roots = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        safe_roots.push(cwd);
    }

    if let Some(home) = dirs::home_dir() {
        safe_roots.push(home.join(".savfox"));
    }

    safe_roots.iter().any(|root| abs.starts_with(root))
}

fn validate_models_section(models: &Value, errors: &mut Vec<ValidationError>) {
    if let Value::Object(map) = models {
        for (id, model) in map {
            if let Value::Object(model_map) = model {
                // Provider is required
                if model_map
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .map_or(true, |s| s.is_empty())
                {
                    errors.push(ValidationError {
                        field: format!("models.{id}.provider"),
                        message: "Provider is required".to_string(),
                        severity: Severity::Error,
                    });
                }

                // model_code is required
                if model_map
                    .get("model_code")
                    .and_then(|v| v.as_str())
                    .map_or(true, |s| s.is_empty())
                {
                    errors.push(ValidationError {
                        field: format!("models.{id}.model_code"),
                        message: "Model code is required".to_string(),
                        severity: Severity::Error,
                    });
                }

                // Temperature range
                if let Some(temp) = model_map.get("temperature") {
                    if let Some(t) = temp.as_f64() {
                        if !(0.0..=2.0).contains(&t) {
                            errors.push(ValidationError {
                                field: format!("models.{id}.temperature"),
                                message: format!(
                                    "Temperature must be between 0.0 and 2.0, got {t}"
                                ),
                                severity: Severity::Error,
                            });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn missing_workspace_path_is_reported() {
        let config = json!({
            "agents": {
                "writer": {
                    "workspace_dir": "__definitely_missing_workspace_dir__"
                }
            }
        });

        let result = validate_config(&config);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|err| {
            err.field == "agents.writer.workspace_dir"
                && err.severity == Severity::Error
                && err.message.contains("does not exist")
        }));
    }

    #[test]
    fn workspace_auto_create_requires_confirmation() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("workspace-a");

        let config = json!({
            "agents": {
                "writer": {
                    "workspace_dir": missing.to_string_lossy().to_string(),
                    "workspace_auto_create": true
                }
            }
        });

        let result = validate_config(&config);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|err| {
            err.field == "agents.writer.workspace_dir"
                && err.severity == Severity::Error
                && err.message.contains("workspace_auto_create_confirmed=true")
        }));
    }

    #[test]
    fn workspace_auto_create_with_confirmation_creates_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("workspace-b");

        let config = json!({
            "agents": {
                "writer": {
                    "workspace_dir": missing.to_string_lossy().to_string(),
                    "workspace_auto_create": true,
                    "workspace_auto_create_confirmed": true
                }
            }
        });

        let result = validate_config(&config);
        assert!(missing.exists());
        assert!(!result.errors.iter().any(
            |err| err.field == "agents.writer.workspace_dir" && err.severity == Severity::Error
        ));
    }

    #[test]
    fn warns_for_workspace_outside_safe_roots() {
        let dir = tempdir().unwrap();
        let config = json!({
            "agents": {
                "writer": {
                    "workspace_dir": dir.path().to_string_lossy().to_string()
                }
            }
        });

        let result = validate_config(&config);
        assert!(result.errors.iter().any(|err| {
            err.field == "agents.writer.workspace_dir"
                && err.severity == Severity::Warning
                && err.message.contains("outside safe roots")
        }));
    }
}
