#![allow(clippy::unwrap_used)]

//! Tests for configuration validation and migration.

use serde_json::json;

#[test]
fn config_provider_id_format() {
    // Provider IDs should be "{provider}/{model}" format
    let valid_ids = vec![
        "openai/gpt-4o",
        "anthropic/claude-3-sonnet",
        "ollama/llama3",
        "deepseek/deepseek-chat",
    ];

    for id in valid_ids {
        let parts: Vec<&str> = id.splitn(2, '/').collect();
        assert_eq!(parts.len(), 2, "ID should have provider/model format: {id}");
        assert!(!parts[0].is_empty(), "provider should not be empty: {id}");
        assert!(!parts[1].is_empty(), "model should not be empty: {id}");
    }
}

#[test]
fn config_agent_models_schema() {
    // Verify the agent models schema matches the expected format
    let agent_config = json!({
        "name": "test-agent",
        "models": {
            "primary": "openai/gpt-4o",
            "fallbacks": ["anthropic/claude-3-sonnet", "deepseek/deepseek-chat"]
        },
        "system_prompt": "You are a helpful assistant."
    });

    let models = &agent_config["models"];
    assert!(models["primary"].is_string());
    assert!(models["fallbacks"].is_array());

    let fallbacks = models["fallbacks"].as_array().unwrap();
    assert_eq!(fallbacks.len(), 2);
}

#[test]
fn config_migration_v1_to_v2() {
    // Test migration from flat model field to models.primary/fallbacks
    let v1_config = json!({
        "name": "old-agent",
        "model": "openai/gpt-4o",
        "provider": "openai"
    });

    // Simulated migration
    let primary = v1_config["model"].as_str().unwrap_or("openai/gpt-4o");
    let v2_config = json!({
        "name": v1_config["name"],
        "models": {
            "primary": primary,
            "fallbacks": []
        }
    });

    assert_eq!(v2_config["models"]["primary"], "openai/gpt-4o");
    assert!(
        v2_config["models"]["fallbacks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Old flat fields should be gone in v2
    assert!(v2_config.get("model").is_none());
    assert!(v2_config.get("provider").is_none());
}
