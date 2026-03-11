# CLI Command Reference

The `savfox` CLI is the primary interface for interacting with the Savfox AI
agent. It dispatches to subcommands for interactive sessions, non-interactive
execution, gateway management, and more.

## Top-level usage

```
savfox [OPTIONS] [PROMPT]
savfox [OPTIONS] <COMMAND> [ARGS]
```

If no subcommand is given, Savfox starts in interactive TUI mode. An optional
positional `PROMPT` argument seeds the initial message.

### Global options

| Flag                  | Type   | Description                                      |
|-----------------------|--------|--------------------------------------------------|
| `-c <KEY=VALUE>`      | String | Config override (repeatable)                     |
| `-m, --model <MODEL>` | String | Override the model                              |
| `-p, --profile <NAME>`| String | Use a config profile                            |
| `--oss`               | Flag   | Use only open-source models                      |
| `--sandbox <MODE>`    | String | Sandbox mode (`workspace-write`, etc.)           |
| `--ask-for-approval <POLICY>` | String | Approval policy (`on-request`, etc.)    |
| `--full-auto`         | Flag   | Skip approval prompts                            |
| `--search`            | Flag   | Enable web search                                |
| `-C, --cwd <DIR>`     | Path   | Override working directory                       |
| `-i, --images <PATHS>`| Paths  | Attach image files (comma-separated)             |
| `--add-dir <DIR>`     | Path   | Add directory to context (repeatable)            |
| `--enable <FEATURE>`  | String | Enable a feature flag (repeatable)               |
| `--disable <FEATURE>` | String | Disable a feature flag (repeatable)              |
| `--dangerously-bypass-approvals-and-sandbox` | Flag | Bypass all safety mechanisms |

---

## Subcommands

### `savfox` (no subcommand)

Start the interactive TUI. Optionally pass a prompt as a positional argument:

```bash
savfox "Explain async/await in Rust"
savfox -m gpt-4o "Fix the failing tests"
```

---

### `savfox exec`

Run Savfox non-interactively. Alias: `savfox e`.

```bash
savfox exec "Add error handling to the parser"
savfox exec --json "List all TODO comments"
```

| Flag          | Description                           |
|---------------|---------------------------------------|
| `--json`      | Output structured JSON                |
| `--quiet`     | Suppress progress output              |

#### `savfox exec resume`

Resume a previous session non-interactively.

```bash
savfox exec resume --last "Continue from where we left off"
savfox exec resume <SESSION_ID>
```

---

### `savfox review`

Run a code review non-interactively.

```bash
savfox review
savfox review --target branch:feature
```

---

### `savfox resume`

Resume a previous interactive session.

```bash
savfox resume              # Show session picker
savfox resume --last       # Resume most recent session
savfox resume <SESSION_ID> # Resume specific session
savfox resume --all        # Show all sessions (disable cwd filtering)
```

| Flag             | Description                           |
|------------------|---------------------------------------|
| `--last`         | Skip picker, resume most recent       |
| `--all`          | Show all sessions regardless of cwd   |
| `<SESSION_ID>`   | Resume a specific session by ID/name  |

---

### `savfox fork`

Fork a previous interactive session.

```bash
savfox fork              # Show session picker
savfox fork --last       # Fork most recent session
savfox fork <SESSION_ID> # Fork specific session
```

| Flag             | Description                           |
|------------------|---------------------------------------|
| `--last`         | Skip picker, fork most recent         |
| `--all`          | Show all sessions regardless of cwd   |

---

### `savfox gateway`

Run or manage the gateway server.

```bash
savfox gateway                           # Start the gateway
savfox gateway --port 8080 --token abc   # Start with custom port/token
savfox gateway --host 0.0.0.0            # Listen on all interfaces
savfox gateway --tls-cert cert.pem --tls-key key.pem
```

#### `savfox gateway start`

Start the gateway as a background daemon.

```bash
savfox gateway start --port 18881 --host 127.0.0.1
savfox gateway start --pid-file /var/run/savfox-gateway.pid
```

#### `savfox gateway stop`

Stop a running gateway daemon.

```bash
savfox gateway stop
savfox gateway stop --pid-file /var/run/savfox-gateway.pid
```

#### `savfox gateway restart`

Restart the gateway daemon (stop then start).

```bash
savfox gateway restart --port 18881
```

#### `savfox gateway status`

Check the status of a running gateway.

```bash
savfox gateway status
savfox gateway status --url http://192.168.1.100:18881
```

#### `savfox gateway logs`

View gateway logs.

```bash
savfox gateway logs
savfox gateway logs --lines 100 --follow
```

| Flag       | Default                         | Description              |
|------------|---------------------------------|--------------------------|
| `--url`    | `http://127.0.0.1:18881`       | Gateway URL              |
| `--lines`  | `50`                            | Number of log lines      |
| `--follow` | `false`                         | Follow logs in real-time |

#### `savfox gateway models`

List available models from the gateway.

```bash
savfox gateway models
savfox gateway models --url http://192.168.1.100:18881
```

#### `savfox gateway approvals`

Manage execution approval requests.

```bash
savfox gateway approvals                    # List pending
savfox gateway approvals list               # List pending
savfox gateway approvals approve <ID>       # Approve by ID
savfox gateway approvals deny <ID>          # Deny by ID
savfox gateway approvals deny <ID> --reason "unsafe command"
```

#### `savfox gateway devices`

Manage device pairing.

```bash
savfox gateway devices                      # List devices
savfox gateway devices list                 # List paired devices
savfox gateway devices pair                 # Generate pairing token
savfox gateway devices pair --name "phone"  # Name the device
savfox gateway devices revoke <ID>          # Revoke a device
```

#### `savfox gateway channels`

List chat channel integrations.

```bash
savfox gateway channels
```

#### `savfox gateway nodes`

List connected nodes.

```bash
savfox gateway nodes
```

#### `savfox gateway install`

Install the gateway as a system service.

```bash
savfox gateway install
savfox gateway install --name custom-gateway
```

Creates a systemd unit on Linux or a launchd plist on macOS.

#### `savfox gateway uninstall`

Remove the gateway system service.

```bash
savfox gateway uninstall
```

---

### `savfox login`

Manage authentication.

```bash
savfox login                            # Interactive login (browser)
savfox login --device-auth              # Device code flow
savfox login --with-api-key             # API key from stdin
savfox login status                     # Show login status
```

Pipe an API key:

```bash
printenv OPENAI_API_KEY | savfox login --with-api-key
```

---

### `savfox logout`

Remove stored authentication credentials.

```bash
savfox logout
```

---

### `savfox mcp`

Manage MCP (Model Context Protocol) servers. Experimental.

```bash
savfox mcp <SUBCOMMAND>
```

---

### `savfox mcp-server`

Run Savfox as an MCP server (stdio transport). Experimental.

```bash
savfox mcp-server
```

---

### `savfox app-server`

Run the app server or related tooling. Experimental.

```bash
savfox app-server                           # Start the app server
savfox app-server --analytics-default-enabled
savfox app-server generate-ts -o ./types    # Generate TypeScript bindings
savfox app-server generate-json-schema -o ./schema
```

---

### `savfox apply`

Apply the latest diff produced by an Savfox agent session as `git apply`.
Alias: `savfox a`.

```bash
savfox apply
```

---

### `savfox send`

Send a message to a chat channel via the gateway.

```bash
savfox send --channel "discord:12345" --text "Hello!"
```

---

### `savfox wizard`

Run the interactive setup wizard.

```bash
savfox wizard
```

---

### `savfox doctor`

Diagnose system health and configuration.

```bash
savfox doctor
```

---

### `savfox migrate`

Migrate configuration from an OpenClaw (TypeScript) installation.

```bash
savfox migrate
```

---

### `savfox features`

Inspect and manage feature flags.

```bash
savfox features list                   # List all features
savfox features enable unified_exec    # Enable a feature
savfox features disable shell_tool     # Disable a feature
```

---

### `savfox completion`

Generate shell completion scripts.

```bash
savfox completion bash > ~/.bash_completion.d/savfox
savfox completion zsh > ~/.zsh/completions/_savfox
savfox completion fish > ~/.config/fish/completions/savfox.fish
savfox completion powershell > savfox.ps1
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

---

### `savfox acp`

Run ACP bridge on stdio, backed by gateway WS-RPC.

```bash
savfox acp --gateway-url http://127.0.0.1:18881 --token "$SAVFOX_TOKEN"
```

Zed example:

```json
{
  "agent_servers": {
    "savfox": {
      "command": "savfox",
      "args": ["acp", "--gateway-url", "http://127.0.0.1:18881", "--token", "${SAVFOX_TOKEN}"]
    }
  }
}
```

---

### `savfox sandbox`

Run commands within a sandboxed environment.

```bash
savfox sandbox macos <COMMAND>    # Seatbelt (macOS)
savfox sandbox linux <COMMAND>    # Landlock + seccomp (Linux)
savfox sandbox windows <COMMAND>  # Restricted token (Windows)
```

---

### `savfox cloud`

Browse tasks from Savfox Cloud and apply changes locally. Experimental.

```bash
savfox cloud
```

---

## Configuration overrides

Config overrides can be passed with `-c` and are applied with highest precedence:

```bash
savfox -c model=gpt-4o -c sandbox_mode=workspace-write "Fix the bug"
savfox exec -c features.unified_exec=true "Run task"
```

Feature flags can also be toggled with `--enable` and `--disable`:

```bash
savfox --enable web_search_request --disable unified_exec "Search for docs"
```

## Environment variables

| Variable       | Description                                  |
|----------------|----------------------------------------------|
| `SAVFOX_HOME`  | Override the Savfox home directory            |
| `TERM`         | Terminal type (affects TUI behavior)          |

The default Savfox home directory is `~/.savfox`.
