# Plugin Development Guide

Savfox plugins extend the gateway with new capabilities: channels, tools, providers, services, and more.

## Plugin Structure

A plugin is a directory with an `savfox.plugin.toml` manifest:

```
my-plugin/
├── savfox.plugin.toml
├── src/
│   └── lib.rs
└── Cargo.toml
```

## Plugin Manifest

```toml
name = "My Plugin"
version = "1.0.0"
description = "Adds custom functionality"
author = "Your Name"
entry_type = "subprocess"
entry_point = "bin/plugin.exe"
isolation_mode = "subprocess" # in_process | subprocess | wasm_sandbox

[[config_fields]]
name = "api_key"
field_type = "string"
label = "API Key"
required = true
is_sensitive = true

[[config_fields]]
name = "mode"
field_type = "select"
label = "Mode"
options = ["safe", "full"]
default_value = "safe"
```

### Plugin Kinds

| Kind | Description |
|------|-------------|
| `channel` | Chat channel (Discord, Slack, etc.) |
| `tool` | Agent tool |
| `provider` | LLM provider |
| `memory` | Memory storage backend (exclusive slot) |
| `service` | Background service |
| `hook` | Message hook |

## Plugin Trait

All plugins implement the `Plugin` trait:

```rust
use savfox_plugin_sdk::{
    Plugin,
    PluginChannelDefinition,
    PluginHookDefinition,
    PluginToolDefinition,
};

pub struct MyPlugin { /* ... */ }

impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }
    fn name(&self) -> &str { "My Plugin" }
    fn version(&self) -> Option<&str> { Some("1.0.0") }

    fn register_channel(&self) -> Vec<PluginChannelDefinition> {
        vec![PluginChannelDefinition {
            name: "discord".to_string(),
            description: "Channel events from Discord".to_string(),
            platforms: vec!["discord".to_string()],
        }]
    }

    fn register_tool(&self) -> Vec<PluginToolDefinition> {
        vec![PluginToolDefinition {
            name: "my_tool".to_string(),
            description: "Run a plugin tool".to_string(),
            parameters: None,
            required: vec![],
        }]
    }

    fn register_hook(&self) -> Vec<PluginHookDefinition> {
        vec![PluginHookDefinition {
            event: "gateway_start".to_string(),
            priority: Some(0),
        }]
    }
}
```

## Plugin Lifecycle

1. **Discovery** — Plugins are auto-discovered from `{savfox_home}/plugins/`
2. **Loading** — Manifest is read and validated
3. **Filtering** — Allow/deny lists and slot assignments applied
4. **Initialization** — `init()` then `start()` are called
5. **Running** — Plugin handles events
6. **Shutdown** — `stop()` called
7. **Uninstall** — `uninstall()` called before removal

## Configuration

Plugins are configured in `config.toml`:

```toml
[plugins]
allow = ["my-plugin"]
deny = []

[plugins.entries.my-plugin]
enabled = true
config = { api_key = "sk-..." }

[plugins.slots]
memory = "memory-lancedb"  # exclusive slot
```

## CLI Commands

```bash
savfox plugins list              # List installed plugins
savfox plugins install <path>    # Install a local plugin
savfox plugins install <id> --registry ./plugins-registry.json
savfox plugins update <id>       # Update one registry plugin
savfox plugins update --all      # Update all installed registry plugins
savfox plugins uninstall <id>    # Remove a plugin
```
