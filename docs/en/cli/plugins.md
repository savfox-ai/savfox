# Plugin Management

Manage Savfox plugins from the command line.

## List Plugins

```bash
savfox plugins list
```

Shows all installed plugins with their status (enabled/disabled), version, and kind.

## Install a Plugin

```bash
savfox plugins install <path-or-id>
```

Install from a local directory:
```bash
savfox plugins install ./my-plugin/
```

## Enable/Disable

```bash
savfox plugins enable <plugin-id>
savfox plugins disable <plugin-id>
```

## Plugin Diagnostics

```bash
savfox plugins doctor
```

Checks:
- Plugin manifest validity
- Required dependencies available
- Configuration schema compliance
- Slot conflicts (exclusive slots like memory)

## Configuration

Plugins are configured in `config.toml`:

```toml
[plugins]
allow = ["memory-lancedb", "my-tool"]
deny = ["experimental-plugin"]

[plugins.entries.memory-lancedb]
enabled = true
config = { db_path = "~/.savfox/lancedb" }

[plugins.slots]
memory = "memory-lancedb"
```

### Allow/Deny Lists

- `allow` — Only these plugins are loaded (whitelist mode)
- `deny` — These plugins are never loaded (blacklist mode)
- If both are empty, all discovered plugins are loaded

### Exclusive Slots

Some plugin kinds have exclusive slots — only one plugin of that kind can be active:

| Slot | Description |
|------|-------------|
| `memory` | Memory storage backend |

## Plugin Kinds

| Kind | Description |
|------|-------------|
| `channel` | Chat bridge (Discord, Slack, etc.) |
| `tool` | Agent tool |
| `provider` | LLM provider |
| `memory` | Memory storage backend |
| `service` | Background service |
| `hook` | Message hook |

## See Also

- [Plugin Development Guide](../tools/plugins.md)
