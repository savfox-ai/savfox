# Configuration

Savfox uses a layered configuration system with support for profiles, CLI overrides, and feature flags.

## Config File Location

```
~/.savfox/config.toml
```

Alternative primary formats are also supported:

```
~/.savfox/config.yaml
~/.savfox/config.yml
```

On Windows:

```
%USERPROFILE%\.savfox\config.toml
%USERPROFILE%\.savfox\config.yaml
%USERPROFILE%\.savfox\config.yml
```

Startup auto-detect order is:

1. `config.toml`
2. `config.yaml`
3. `config.yml`

If multiple files exist, the highest-priority file above is used.

## YAML Format

Savfox accepts native YAML config files with the same schema as TOML:

```yaml
model: gpt-4o

sandbox_mode: workspace-write

gateway:
  host: 127.0.0.1
  port: 18881
```

Use the CLI to migrate from TOML to YAML:

```bash
savfox config convert --to yaml --output ~/.savfox/config.yaml
```

When converting from TOML, Savfox preserves the leading comment block when possible.

## Environment Variable Substitution

String values support environment-variable expansion before validation:

- `${VAR}`: use `VAR`, empty string when unset.
- `${VAR:-default}`: use `default` when `VAR` is unset or empty.
- `${VAR:?error message}`: fail config loading if `VAR` is unset or empty.

Examples:

```toml
model = "${SAVFOX_MODEL:-gpt-4o}"
gateway_token = "${SAVFOX_GATEWAY_TOKEN:?SAVFOX_GATEWAY_TOKEN is required}"
```

```yaml
model: ${SAVFOX_MODEL:-gpt-4o}
gateway_token: ${SAVFOX_GATEWAY_TOKEN:?SAVFOX_GATEWAY_TOKEN is required}
```

Supported variables:

- Any environment variable available to the running Savfox process is supported.
- Common examples in config files: `SAVFOX_MODEL`, `SAVFOX_GATEWAY_TOKEN`, `SAVFOX_HOME`, `PATH`, `HOME`/`USERPROFILE`.

## Config Sections

### Authentication

```toml
[auth]
provider = "openai"        # or "chatgpt", "ollama", "lmstudio", etc.
api_key = "sk-..."         # API key (if using API key auth)
```

For ChatGPT-managed authentication (recommended), use `savfox login` instead of setting API keys manually.

### Model Selection

```toml
[model]
provider_id = "openai"
model = "gpt-4o"
```

### Sandbox Policy

```toml
[sandbox]
mode = "workspace-write"   # "read-only", "workspace-write", "full-access"
```

### Gateway Server

```toml
[gateway]
port = 18881
host = "127.0.0.1"
token = "my-secret-token"  # omit for auto-generated
```

### Chat Bridges

```toml
[gateway.bridges.discord]
enabled = true
bot_token = "..."

[gateway.bridges.telegram]
enabled = true
bot_token = "..."

[gateway.bridges.slack]
enabled = true
bot_token = "..."
signing_secret = "..."
```

### MCP Servers

```toml
[mcp.servers.my-server]
command = "node"
args = ["server.js"]
```

### Skills

```toml
[skills]
enabled = true
# paths = ["./custom-skills/"]
```

### Shell Preferences

```toml
[shell]
# Terminal/shell preferences
```

### Git Integration

```toml
[git]
# Git-related settings
```

## CLI Overrides

Override any config value from the command line with `-c`:

```bash
savfox -c model.model=gpt-4o exec "Hello"
savfox -c sandbox.mode=read-only exec "Read this file"
savfox -c gateway.port=9090 gateway
```

Multiple overrides can be stacked:

```bash
savfox -c model.model=gpt-4o -c sandbox.mode=full-access exec "Do something"
```

## Configuration Profiles

Use named profiles to switch between different configurations:

```bash
savfox --profile work exec "Task for work project"
savfox --profile personal exec "Personal project task"
savfox -p oss exec "Use OSS models"
```

## Feature Flags

Feature flags control access to experimental and in-development functionality.

### Stages

- **under-development** — Not yet ready for general use
- **experimental** — Available but may change
- **stable** — Production-ready
- **deprecated** — Will be removed in a future version

### Managing Feature Flags

From the CLI:

```bash
savfox features              # list all flags and their status
savfox features enable <F>   # enable a feature
savfox features disable <F>  # disable a feature
```

Or via command-line flags:

```bash
savfox --enable my-feature exec "Use new feature"
savfox --disable some-feature exec "Without this feature"
```

## Config Layer Priority

Configuration is resolved in this order (later overrides earlier):

1. **System defaults** — Built-in defaults
2. **User config** — `~/.savfox/config.toml`
3. **Workspace config** — Project-level `.savfox/config.toml`
4. **Profile** — Named profile overrides
5. **CLI overrides** — `-c key=value` flags
6. **Cloud requirements** — Enterprise/managed settings (if applicable)

## JSON Schema

Export the config schema for editor integration:

```bash
savfox app-server generate-json-schema -o ./schema
```

This produces a JSON Schema file that editors can use for autocompletion and validation of `config.toml`.
