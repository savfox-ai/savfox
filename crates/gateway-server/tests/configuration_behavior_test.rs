//! Exercise the production gateway configuration parser and validator.
#![allow(clippy::unwrap_used)]

use pretty_assertions::assert_eq;
use savfox_gateway_server::config::{GatewayConfig, Severity, validate_config};
use serde_json::json;

#[test]
fn gateway_config_deserialization_preserves_secure_defaults() {
    let config: GatewayConfig = serde_json::from_value(json!({"port": 19000})).unwrap();
    assert_eq!(config.port, 19000);
    assert!(config.host.is_loopback());
    assert!(!config.trust_x_forwarded_for);
    assert_eq!(config.rate_limit.max_requests, 100);
    assert_eq!(config.rate_limit.max_connections_per_ip, 10);
}

#[test]
fn production_validator_rejects_invalid_ports_and_weak_credentials() {
    for port in [json!(0), json!(65536), json!("18881")] {
        let result = validate_config(&json!({"gateway": {"port": port}}));
        assert!(!result.valid);
        assert!(
            result.errors.iter().any(|error| {
                error.field == "gateway.port" && error.severity == Severity::Error
            })
        );
    }
    for token in ["", "test", " short-token "] {
        let result = validate_config(&json!({"gateway": {"token": token}}));
        assert!(!result.valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.field == "gateway.token")
        );
    }
}

#[test]
fn production_validator_accepts_secret_references_and_valid_settings() {
    let result = validate_config(&json!({
        "gateway": {"host": "127.0.0.1", "port": 18881, "token": "${SAVFOX_GATEWAY_TOKEN}"}
    }));
    assert!(result.valid, "{:?}", result.errors);
    assert!(result.errors.is_empty());
}

#[test]
fn production_validator_rejects_out_of_range_agent_temperature() {
    let result = validate_config(&json!({"agents": {"worker": {"temperature": 3.0}}}));
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.field == "agents.worker.temperature")
    );
}
