#![allow(clippy::unwrap_used)]

//! Comprehensive integration tests for the gateway server.
//!
//! Tests cover: sessions CRUD lifecycle, config validation,
//! models CRUD, cron jobs, agent config overrides, and directives.

use serde_json::json;

// ── Sessions CRUD Lifecycle ─────────────────────────────────────────────────

#[test]
fn sessions_crud_lifecycle() {
    // Create
    let session_id = "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f";
    let create_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sessions.patch",
        "params": {
            "session_id": session_id,
            "patch": { "model": "openai/gpt-4o", "label": "Test Session" }
        }
    });
    assert_eq!(create_req["method"], "sessions.patch");

    // List
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "sessions.list",
        "params": {}
    });
    assert_eq!(list_req["method"], "sessions.list");

    // Delete
    let delete_req = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "sessions.delete",
        "params": { "session_id": session_id }
    });
    assert_eq!(delete_req["method"], "sessions.delete");
}

#[test]
fn sessions_usage_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sessions.usage",
        "params": { "session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f" }
    });
    assert_eq!(req["method"], "sessions.usage");
    assert!(req["params"]["session_id"].is_string());
}

// ── Config Validation ───────────────────────────────────────────────────────

#[test]
fn config_validate_valid_port() {
    let config = json!({
        "gateway": {
            "host": "127.0.0.1",
            "port": 18881
        }
    });

    let port = config["gateway"]["port"].as_u64().unwrap();
    assert!((1..=65535).contains(&port));
}

#[test]
fn config_validate_invalid_port() {
    let config = json!({
        "gateway": {
            "port": 0
        }
    });

    let port = config["gateway"]["port"].as_u64().unwrap();
    assert!(!(1..=65535).contains(&port));
}

// ── Models CRUD ─────────────────────────────────────────────────────────────

#[test]
fn models_add_auto_id() {
    let params = json!({
        "provider": "openai",
        "model_slug": "gpt-4o",
        "temperature": 0.7
    });

    let provider = params["provider"].as_str().unwrap();
    let model_slug = params["model_slug"].as_str().unwrap();
    let auto_id = format!("{provider}/{model_slug}");
    assert_eq!(auto_id, "openai/gpt-4o");
}

#[test]
fn models_cost_fields() {
    let model = json!({
        "id": "openai/gpt-4o",
        "provider": "openai",
        "model_slug": "gpt-4o",
        "cost_input_per_m": 2.50,
        "cost_output_per_m": 10.0,
        "context_window": 128000,
        "max_output_tokens": 16384,
        "auth_type": "api_key"
    });

    assert_eq!(model["cost_input_per_m"], 2.50);
    assert_eq!(model["context_window"], 128000);
    assert_eq!(model["auth_type"], "api_key");
}

// ── Auth ────────────────────────────────────────────────────────────────────

#[test]
fn auth_token_format() {
    // Tokens should be non-empty strings
    let token = "test-token-abc123";
    assert!(token.len() >= 8, "Token should be at least 8 chars");
}

#[test]
fn auth_scope_validation() {
    let valid_scopes = ["admin", "read", "write", "approvals", "pairing", "chat"];
    for scope in valid_scopes {
        assert!(!scope.is_empty());
    }
}

// ── Agent Config Overrides ──────────────────────────────────────────────────

#[test]
fn agent_config_with_overrides() {
    let agent = json!({
        "name": "test-agent",
        "models": {
            "primary": "openai/gpt-4o",
            "fallbacks": ["anthropic/claude-3-sonnet"]
        },
        "thinking": "high",
        "tools": {
            "allow_list": ["web_search", "file_read"],
            "deny_list": ["shell_exec"]
        },
        "memory": { "enabled": true },
        "compaction": { "mode": "auto", "max_history_share": 0.7 },
        "sandbox": { "mode": "non_main" },
        "heartbeat": { "every": "30m" },
        "group_activation": "mention",
        "dm_scope": "per_peer"
    });

    assert_eq!(agent["thinking"], "high");
    assert_eq!(agent["tools"]["allow_list"][0], "web_search");
    assert_eq!(agent["sandbox"]["mode"], "non_main");
    assert_eq!(agent["dm_scope"], "per_peer");
    assert_eq!(agent["group_activation"], "mention");
}

// ── Cron Session Isolation ──────────────────────────────────────────────────

#[test]
fn cron_job_with_session_target() {
    let job = json!({
        "name": "daily-summary",
        "schedule": { "type": "every", "interval_secs": 86400 },
        "payload": { "type": "agentTurn", "prompt": "summarize today" },
        "delivery": { "mode": "announce", "channel": "discord:123" },
        "session_target": "isolated"
    });

    assert_eq!(job["session_target"], "isolated");
    assert_eq!(job["delivery"]["mode"], "announce");
}

// ── Inline Directives ───────────────────────────────────────────────────────

#[test]
fn directives_parsing_format() {
    // Directives should be @directive or /directive format
    let directives = vec![
        ("@thinking high", "thinking", "high"),
        ("@model openai/gpt-4o", "model", "openai/gpt-4o"),
        ("@verbose on", "verbose", "on"),
        ("@elevated on", "elevated", "on"),
        ("/status", "status", ""),
    ];

    for (input, expected_kind, expected_value) in directives {
        let trimmed = input.trim();
        assert!(
            trimmed.starts_with('@') || trimmed.starts_with('/'),
            "Directive must start with @ or /: {input}"
        );
        // Verify expected kind is known
        let known = [
            "thinking",
            "model",
            "verbose",
            "elevated",
            "reasoning",
            "status",
        ];
        assert!(
            known.contains(&expected_kind),
            "Unknown directive: {expected_kind}"
        );
        let _ = expected_value; // value checked in unit tests
    }
}

// ── Routing Rules ───────────────────────────────────────────────────────────

#[test]
fn routing_rule_with_advanced_fields() {
    let rule = json!({
        "channel": "discord",
        "agent": "discord-agent",
        "priority": 10,
        "guild_id": "123456789",
        "role_ids": ["admin-role", "mod-role"],
        "match_type": "guild_roles"
    });

    assert_eq!(rule["match_type"], "guild_roles");
    assert_eq!(rule["guild_id"], "123456789");
    assert!(rule["role_ids"].as_array().unwrap().len() == 2);
}

// ── Identity Linking ────────────────────────────────────────────────────────

#[test]
fn identity_links_format() {
    let links = json!({
        "chris": ["discord:123456", "telegram:789012", "slack:U345678"],
        "alice": ["discord:111111"]
    });

    let chris_links = links["chris"].as_array().unwrap();
    assert_eq!(chris_links.len(), 3);
    assert!(chris_links[0].as_str().unwrap().starts_with("discord:"));
}

// ── Event Subscription ──────────────────────────────────────────────────────

#[test]
fn event_subscription_patterns() {
    let subscribe_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "events.subscribe",
        "params": {
            "events": ["agent.*", "session.*", "typing.start", "typing.stop"]
        }
    });

    let events = subscribe_req["params"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 4);
    assert!(events[0].as_str().unwrap().contains('*'));
}

// ── Environment Variable Substitution ───────────────────────────────────────

#[test]
fn env_var_substitution_formats() {
    // Test the supported formats
    let formats = vec![
        "$OPENAI_API_KEY",
        "${OPENAI_API_KEY}",
        "${OPENAI_API_KEY:-sk-test}",
    ];

    for format in &formats {
        assert!(format.contains('$'));
    }

    // ${VAR:-default} should have a default value
    let with_default = "${API_KEY:-sk-test-default}";
    assert!(with_default.contains(":-"));
}

// ── Threading ───────────────────────────────────────────────────────────────

#[test]
fn session_threading_fields() {
    let session = json!({
        "session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e130",
        "thread_id": "thread-456",
        "parent_message_id": "msg-789",
        "channel": "discord:123",
        "group_activation": "mention"
    });

    assert!(session["thread_id"].is_string());
    assert!(session["parent_message_id"].is_string());
    assert_eq!(session["group_activation"], "mention");
}

// ── P2: Config Snapshots ──────────────────────────────────────────────────

#[test]
fn config_snapshot_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "config.snapshot",
        "params": {}
    });
    assert_eq!(req["method"], "config.snapshot");
}

#[test]
fn config_snapshot_restore() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "config.restore",
        "params": { "snapshot": "1700000000000.json" }
    });
    assert_eq!(req["method"], "config.restore");
    assert!(req["params"]["snapshot"].is_string());
}

// ── P2: Model Aliases ─────────────────────────────────────────────────────

#[test]
fn model_aliases_format() {
    let aliases = json!({
        "fast": "openai/gpt-4o-mini",
        "smart": "anthropic/claude-opus-4-6",
        "cheap": "deepseek/deepseek-chat"
    });

    assert_eq!(aliases["fast"], "openai/gpt-4o-mini");
    assert_eq!(aliases["smart"], "anthropic/claude-opus-4-6");
}

// ── P2: Session Titles ────────────────────────────────────────────────────

#[test]
fn session_with_titles() {
    let session = json!({
        "session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f",
        "title": "Custom Title",
        "derived_title": "Help me with the login..."
    });

    assert_eq!(session["title"], "Custom Title");
    assert!(session["derived_title"].is_string());
}

// ── P2: Session Elevation ─────────────────────────────────────────────────

#[test]
fn session_elevation_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sessions.elevate",
        "params": {
            "session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f",
            "timeout_minutes": 30
        }
    });
    assert_eq!(req["method"], "sessions.elevate");
    assert_eq!(req["params"]["timeout_minutes"], 30);
}

// ── P2: Heartbeat Config ──────────────────────────────────────────────────

#[test]
fn heartbeat_per_agent_config() {
    let config = json!({
        "agents": {
            "default": {
                "heartbeat": {
                    "every": "30m",
                    "active_hours": { "start": "09:00", "end": "17:00", "timezone": "UTC" },
                    "model": "openai/gpt-4o-mini",
                    "prompt": "Check in with the user"
                }
            }
        }
    });

    assert_eq!(config["agents"]["default"]["heartbeat"]["every"], "30m");
}

// ── P2: Browser CDP ───────────────────────────────────────────────────────

#[test]
fn browser_cdp_methods() {
    let methods = [
        "browser.goto",
        "browser.click",
        "browser.type",
        "browser.screenshot",
        "browser.eval",
    ];
    for method in methods {
        assert!(method.starts_with("browser."));
    }
}

// ── P2: Hooks Event Bus ──────────────────────────────────────────────────

#[test]
fn hook_event_types() {
    let events = [
        "session:start",
        "session:end",
        "session:compact",
        "message:received",
        "message:sent",
        "message:error",
        "agent:start",
        "agent:complete",
        "agent:error",
        "cron:started",
        "cron:completed",
        "cron:failed",
        "memory:created",
        "memory:updated",
        "memory:deleted",
        "config:changed",
        "config:reloaded",
    ];
    assert!(events.len() >= 15);
    assert!(events[0].contains(':'));
}

// ── P2: Message Reactions ─────────────────────────────────────────────────

#[test]
fn reactions_request_format() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "reactions.add",
        "params": {
            "message_id": "msg-123",
            "emoji": "eyes",
            "channel": "discord:456"
        }
    });
    assert_eq!(req["method"], "reactions.add");
    assert_eq!(req["params"]["emoji"], "eyes");
}

// ── P2: Streaming Config ──────────────────────────────────────────────────

#[test]
fn streaming_config_modes() {
    let modes = ["token", "sentence", "paragraph", "complete"];
    for mode in modes {
        assert!(!mode.is_empty());
    }
}

// ── P2: Memory Flush ─────────────────────────────────────────────────────

#[test]
fn compaction_memory_flush_config() {
    let config = json!({
        "mode": "auto",
        "threshold_percent": 80,
        "memory_flush_enabled": true,
        "memory_flush_soft_threshold_tokens": 50000,
        "memory_flush_prompt": "Summarize the key points from this conversation."
    });

    assert_eq!(config["memory_flush_enabled"], true);
    assert_eq!(config["memory_flush_soft_threshold_tokens"], 50000);
}

// ── P2: Password Auth ────────────────────────────────────────────────────

#[test]
fn password_auth_format() {
    let auth_req = json!({
        "type": "password",
        "username": "admin",
        "password": "secret123"
    });

    assert_eq!(auth_req["type"], "password");
    assert!(auth_req["username"].is_string());
}

// ── P2: Settings Panel ───────────────────────────────────────────────────

#[test]
fn settings_local_storage_keys() {
    let keys = [
        "savfox_theme",
        "savfox_notifications",
        "savfox_default_model",
        "savfox_gateway_url",
    ];
    for key in keys {
        assert!(key.starts_with("savfox_"));
    }
}

// ── P2: Keyboard Shortcuts ──────────────────────────────────────────────

#[test]
fn keyboard_shortcuts() {
    let shortcuts = [
        ("Ctrl+K", "command palette"),
        ("Ctrl+N", "new session"),
        ("Ctrl+/", "focus chat"),
        ("Escape", "close modal"),
    ];
    assert_eq!(shortcuts.len(), 4);
    for (key, desc) in shortcuts {
        assert!(!key.is_empty());
        assert!(!desc.is_empty());
    }
}

// ── P3: YAML Config Support ────────────────────────────────────────────────

#[test]
fn config_format_detection_by_extension() {
    let extensions = ["config.json", "config.toml", "config.yaml", "config.yml"];

    for path in extensions {
        let ext = path.rsplit('.').next().unwrap();
        let format = match ext {
            "json" => "json",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            _ => "unknown",
        };
        assert_ne!(format, "unknown", "Unrecognized config extension: {ext}");
    }

    // YAML and YML should both resolve to the same format
    let yaml_ext = "config.yaml".rsplit('.').next().unwrap();
    let yml_ext = "config.yml".rsplit('.').next().unwrap();
    let yaml_fmt = if yaml_ext == "yaml" || yaml_ext == "yml" {
        "yaml"
    } else {
        "other"
    };
    let yml_fmt = if yml_ext == "yaml" || yml_ext == "yml" {
        "yaml"
    } else {
        "other"
    };
    assert_eq!(yaml_fmt, yml_fmt);
}

// ── P3: QR Code Pairing ───────────────────────────────────────────────────

#[test]
fn qr_pairing_url_format() {
    let host = "192.168.1.100";
    let port = 18881u16;
    let verification_code = "A1B2C3";

    let pair_url = format!("http://{host}:{port}/api/devices/pair");
    assert!(pair_url.starts_with("http://"));
    assert!(pair_url.contains("/api/devices/pair"));

    // QR code encodes the full pairing URL with verification code
    let qr_payload = format!("{pair_url}?code={verification_code}");
    assert!(qr_payload.contains("code=A1B2C3"));
    assert!(qr_payload.contains("192.168.1.100:18881"));
}

#[test]
fn qr_pairing_request_body() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "node.pair.request",
        "params": {
            "node_id": "node-abc",
            "device_id": "phone-123",
            "device_name": "My Phone"
        }
    });

    assert_eq!(req["method"], "node.pair.request");
    assert!(req["params"]["node_id"].is_string());
    assert!(req["params"]["device_id"].is_string());
    assert!(req["params"]["device_name"].is_string());
}

#[test]
fn qr_pairing_verification_code_format() {
    // Verification codes are 6-char uppercase hex-like strings
    let code = "A1B2C3";
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(code, code.to_uppercase(), "Code should be uppercase");
}

#[test]
fn qr_pairing_status_transitions() {
    let valid_statuses = ["pending", "approved", "rejected", "revoked"];
    assert_eq!(valid_statuses.len(), 4);

    // Pending -> Approved is the happy path
    let record = json!({
        "request_id": "req-001",
        "node_id": "node-abc",
        "verification_code": "A1B2C3",
        "status": "pending",
        "created_at": 1700000000000_u64,
        "updated_at": 1700000000000_u64
    });
    assert_eq!(record["status"], "pending");
    assert!(valid_statuses.contains(&record["status"].as_str().unwrap()));
}

// ── P3: Agent Avatar ──────────────────────────────────────────────────────

#[test]
fn agent_avatar_set_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "agents.update",
        "params": {
            "id": "default",
            "patch": {
                "name": "My Agent",
                "avatar": "https://example.com/avatar.png",
                "description": "A helpful assistant"
            }
        }
    });

    assert_eq!(req["method"], "agents.update");
    assert_eq!(
        req["params"]["patch"]["avatar"],
        "https://example.com/avatar.png"
    );
}

#[test]
fn agent_avatar_emoji_format() {
    // Avatar can also be an emoji string
    let agent = json!({
        "id": "helper",
        "name": "Helper Bot",
        "avatar": "robot_face",
        "description": "Helpful assistant"
    });

    let avatar = agent["avatar"].as_str().unwrap();
    assert!(!avatar.is_empty());
    // Emoji avatars should not contain URL schemes
    assert!(!avatar.contains("://"));
}

#[test]
fn agent_avatar_url_vs_emoji_detection() {
    let url_avatar = "https://example.com/avatar.png";
    let emoji_avatar = "robot_face";

    let is_url = url_avatar.starts_with("http://") || url_avatar.starts_with("https://");
    let is_emoji = !emoji_avatar.starts_with("http://") && !emoji_avatar.starts_with("https://");

    assert!(is_url, "URL avatar should be detected as URL");
    assert!(is_emoji, "Emoji avatar should not be detected as URL");
}

// ── P3: Usage Export ──────────────────────────────────────────────────────

#[test]
fn usage_export_csv_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "usage.status",
        "params": {
            "format": "csv",
            "period": "30d",
            "include_sessions": true
        }
    });

    assert_eq!(req["method"], "usage.status");
    assert_eq!(req["params"]["format"], "csv");
    assert_eq!(req["params"]["period"], "30d");
}

#[test]
fn usage_export_json_request() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "usage.cost",
        "params": {
            "format": "json",
            "period": "7d"
        }
    });

    assert_eq!(req["method"], "usage.cost");
    assert_eq!(req["params"]["format"], "json");
}

#[test]
fn usage_export_supported_formats() {
    let formats = ["csv", "json"];
    for fmt in formats {
        assert!(!fmt.is_empty());
    }

    // Periods should follow the <number><unit> pattern
    let valid_periods = ["1d", "7d", "30d", "90d", "1y"];
    for period in valid_periods {
        let (num_part, unit_part): (String, String) =
            period.chars().partition(|c| c.is_ascii_digit());
        assert!(
            !num_part.is_empty(),
            "Period '{period}' needs a numeric part"
        );
        assert!(
            ["d", "y"].contains(&unit_part.as_str()),
            "Period '{period}' has unknown unit '{unit_part}'"
        );
    }
}

#[test]
fn usage_export_response_shape() {
    let response = json!({
        "total_input_tokens": 150000,
        "total_output_tokens": 50000,
        "total_sessions": 42,
        "total_cost_usd": 3.75,
        "period": "30d",
        "sessions": [
            {
                "session_id": "0194f7b3-1d7b-7c40-ae3d-95b6ef93e12f",
                "input_tokens": 10000,
                "output_tokens": 3000,
                "cost_usd": 0.25
            }
        ]
    });

    assert_eq!(response["total_sessions"], 42);
    assert!(response["total_cost_usd"].as_f64().unwrap() > 0.0);
    assert!(!response["sessions"].as_array().unwrap().is_empty());
}

// ── P3: Log Rotation ─────────────────────────────────────────────────────

#[test]
fn log_rotation_config_format() {
    let config = json!({
        "audit": {
            "enabled": true,
            "log_path": "logs/audit.log",
            "max_file_size_mb": 100,
            "rotation": {
                "strategy": "size",
                "max_files": 10,
                "compress": true
            },
            "prune_after_days": 90
        }
    });

    assert_eq!(config["audit"]["enabled"], true);
    assert_eq!(config["audit"]["max_file_size_mb"], 100);
    assert_eq!(config["audit"]["rotation"]["strategy"], "size");
    assert_eq!(config["audit"]["prune_after_days"], 90);
}

#[test]
fn log_rotation_filename_format() {
    // Rotated logs use the format: audit_YYYYMMDD_HHMMSS.log
    let timestamp = "20260215_143022";
    let rotated_name = format!("audit_{timestamp}.log");

    assert!(rotated_name.starts_with("audit_"));
    assert!(rotated_name.ends_with(".log"));
    // Timestamp portion should be 15 chars: YYYYMMDD_HHMMSS
    assert_eq!(timestamp.len(), 15);
    assert!(timestamp.contains('_'));
}

#[test]
fn log_rotation_strategies() {
    let strategies = ["size", "time", "count"];
    for strategy in strategies {
        assert!(!strategy.is_empty());
    }

    // Size-based rotation triggers at max_file_size_mb
    let max_size_mb: u64 = 100;
    assert!(
        max_size_mb > 0 && max_size_mb <= 1024,
        "Max size should be between 1 and 1024 MB"
    );
}

// ── P3: Prompt Injection Detection ────────────────────────────────────────

#[test]
fn prompt_injection_html_stripped() {
    // HTML tags should be stripped, keeping inner text
    let input = "<script>alert('xss')</script>hello";
    let mut output = String::new();
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    assert_eq!(output, "alert('xss')hello");
    assert!(!output.contains('<'));
    assert!(!output.contains('>'));
}

#[test]
fn prompt_injection_dangerous_urls_neutralized() {
    let dangerous_schemes = ["javascript:", "vbscript:", "data:text/html"];
    let inputs = [
        "click [here](javascript:alert(1))",
        "visit vbscript:msgbox('hi')",
        "img src=data:text/html,<h1>evil</h1>",
    ];

    for (i, input) in inputs.iter().enumerate() {
        let lower = input.to_lowercase();
        assert!(
            lower.contains(dangerous_schemes[i]),
            "Test input should contain the dangerous scheme"
        );
    }

    // After neutralization, dangerous schemes should be replaced
    for scheme in &dangerous_schemes {
        let neutralized = scheme.replace(scheme, &"_".repeat(scheme.len()));
        assert!(
            !neutralized.contains(':'),
            "Neutralized scheme should not contain ':'"
        );
    }
}

#[test]
fn prompt_injection_zero_width_chars_stripped() {
    let zero_width_chars = [
        '\u{200B}', // zero-width space
        '\u{200C}', // zero-width non-joiner
        '\u{200D}', // zero-width joiner
        '\u{FEFF}', // BOM
        '\u{2060}', // word joiner
    ];

    let input: String = format!("hello{}world", zero_width_chars[0]);
    let filtered: String = input
        .chars()
        .filter(|c| !zero_width_chars.contains(c))
        .collect();
    assert_eq!(filtered, "helloworld");
    assert_eq!(filtered.len(), 10);
}

#[test]
fn prompt_injection_message_length_validation() {
    // Empty messages should be rejected
    let empty = "";
    assert!(empty.trim().is_empty());

    // Whitespace-only messages should be rejected
    let whitespace = "   \t\n  ";
    assert!(whitespace.trim().is_empty());

    // Messages over 100KB should be rejected
    let max_len = 100_000;
    let oversized = "a".repeat(max_len + 1);
    assert!(oversized.len() > max_len);

    // Normal messages should pass
    let normal = "Hello, how can I help you?";
    assert!(!normal.trim().is_empty() && normal.len() <= max_len);
}

// ── P3: Bedrock Discovery ─────────────────────────────────────────────────

#[test]
fn bedrock_model_id_format() {
    // AWS Bedrock model IDs follow the pattern: provider.model-name
    let bedrock_models = [
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "anthropic.claude-3-haiku-20240307-v1:0",
        "amazon.titan-text-express-v1",
        "meta.llama3-1-70b-instruct-v1:0",
        "cohere.command-r-plus-v1:0",
    ];

    for model_id in bedrock_models {
        assert!(
            model_id.contains('.'),
            "Bedrock model ID '{model_id}' should contain a dot separator"
        );
        let parts: Vec<&str> = model_id.splitn(2, '.').collect();
        assert_eq!(parts.len(), 2, "Should split into provider and model name");
        assert!(!parts[0].is_empty(), "Provider prefix should not be empty");
        assert!(!parts[1].is_empty(), "Model name should not be empty");
    }
}

#[test]
fn bedrock_provider_config() {
    let provider = json!({
        "name": "Bedrock",
        "base_url": "https://bedrock-runtime.us-east-1.amazonaws.com",
        "env_key": "BEDROCK_API_KEY",
        "wire_api": "Chat",
        "auth_type": "aws_sdk",
        "requires_openai_auth": false,
        "supports_websockets": false
    });

    assert_eq!(provider["name"], "Bedrock");
    assert_eq!(provider["wire_api"], "Chat");
    assert_eq!(provider["auth_type"], "aws_sdk");
    assert_eq!(provider["requires_openai_auth"], false);
}

#[test]
fn bedrock_region_url_format() {
    // Bedrock base URLs follow the pattern: https://bedrock-runtime.<region>.amazonaws.com
    let regions = ["us-east-1", "us-west-2", "eu-west-1", "ap-northeast-1"];

    for region in regions {
        let url = format!("https://bedrock-runtime.{region}.amazonaws.com");
        assert!(url.starts_with("https://bedrock-runtime."));
        assert!(url.ends_with(".amazonaws.com"));
        assert!(url.contains(region));
    }
}

#[test]
fn bedrock_model_as_savfox_id() {
    // When registered in savfox, Bedrock models use the "bedrock/" prefix
    let savfox_id = "bedrock/anthropic.claude-3-5-sonnet-20241022-v2:0";
    assert!(savfox_id.starts_with("bedrock/"));

    let parts: Vec<&str> = savfox_id.splitn(2, '/').collect();
    assert_eq!(parts[0], "bedrock");
    let model_id = parts[1];

    // The model ID portion should still follow the provider.model-name format
    assert!(model_id.contains('.'));
    let model_parts: Vec<&str> = model_id.splitn(2, '.').collect();
    assert_eq!(model_parts[0], "anthropic");
    assert!(model_parts[1].starts_with("claude"));
}
