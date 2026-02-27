# CLI Reference

## Global Options

These options apply to all subcommands:

| Flag | Description |
|------|-------------|
| `-m, --model <MODEL>` | Specify the LLM model to use |
| `-c, --config <KEY=VALUE>` | Override a config value (repeatable) |
| `-p, --profile <PROFILE>` | Use a named configuration profile |
| `--oss` | Use open-source local provider (Ollama, LM Studio) |
| `--search` | Enable web search mode |
| `--sandbox <MODE>` | Sandbox policy: `read-only`, `workspace-write`, `full-access` |
| `--ask-for-approval <POLICY>` | Approval policy: `never`, `on-request`, `unless-trusted`, `on-failure` |
| `--full-auto` | Low-friction mode (`--ask-for-approval on-request --sandbox workspace-write`) |
| `--yolo` | Skip all confirmations and sandboxing (**dangerous**) |
| `-C, --cd <DIR>` | Set working directory |
| `-i, --image <FILE>` | Attach images (comma-separated paths) |
| `--add-dir <DIR>` | Additional writable directories for sandbox |
| `--enable <FEATURE>` | Enable a feature flag (repeatable) |
| `--disable <FEATURE>` | Disable a feature flag (repeatable) |

## Subcommands

### `exec` (alias: `e`)

Run a task non-interactively.

```bash
savfox exec "Add input validation to the signup form"
savfox e "Explain what this function does"
```

**Options:**

| Flag | Description |
|------|-------------|
| `--json` | Output events as JSONL (one JSON object per line) |
| `--color <MODE>` | Color output: `auto`, `always`, `never` |
| `--output-last-message <FILE>` | Save the agent's final message to a file |
| `--output-schema <FILE>` | JSON Schema file defining expected response shape |

### `review`

Run a non-interactive code review.

```bash
savfox review
savfox review "Focus on security issues"
```

### `resume`

Resume a previous interactive session.

```bash
savfox resume              # interactive session picker
savfox resume --last       # resume the most recent session
savfox resume <SESSION_ID> # resume a specific session
```

### `fork`

Fork from a previous session to explore a different direction.

```bash
savfox fork
```

### `apply` (alias: `a`)

Apply the most recent agent-generated diff to your working directory using `git apply`.

```bash
savfox apply
```

### `login` / `logout`

Manage authentication credentials.

```bash
savfox login       # interactive login flow
savfox --oss login # login for local OSS providers
savfox logout      # remove stored credentials
```

### `gateway`

Start or manage the gateway server for remote access.

```bash
savfox gateway                          # start the server
savfox gateway --port 8080 --token abc  # custom port and token
savfox gateway status                   # check server health
savfox gateway logs --follow            # stream logs
savfox gateway models                   # list available models
```

See [Gateway](gateway.md) for full details.

**Management subcommands:**

| Subcommand | Description |
|------------|-------------|
| `status` | Check gateway health |
| `logs [--follow] [--lines N]` | View or stream logs |
| `models` | List available LLM models |
| `approvals {list\|approve\|deny}` | Manage execution approvals |
| `devices {list\|pair\|revoke}` | Manage device pairing |
| `channels` | Manage chat bridge channels |
| `nodes` | Manage connected nodes |

### `mcp-server`

Run Savfox as an MCP (Model Context Protocol) server over stdio.

```bash
savfox mcp-server
```

See [MCP Server](mcp-server.md) for details.

### `mcp`

Manage MCP server configurations.

```bash
savfox mcp
```

### `app-server`

Run the app server (JSON-RPC over stdio), used by IDE extensions.

```bash
savfox app-server
```

**Code generation subcommands:**

```bash
savfox app-server generate-ts -o ./types       # generate TypeScript bindings
savfox app-server generate-json-schema -o ./schema  # generate JSON Schema
```

### `sandbox`

Run a command under platform-specific sandboxing.

```bash
savfox sandbox macos <command>    # macOS Seatbelt
savfox sandbox linux <command>    # Linux Landlock + seccomp
savfox sandbox windows <command>  # Windows restricted token
```

See [Sandbox](sandbox.md) for details.

### `features`

Inspect and manage feature flags.

```bash
savfox features              # list all feature flags
savfox features enable <F>   # enable a feature
savfox features disable <F>  # disable a feature
```

### `config`

Manage gateway-side configuration via WS-RPC.

```bash
savfox config validate
savfox config export --format yaml --output config.yaml
savfox config convert --to yaml --output config.yaml
```

### `cloud` (alias: `cloud-tasks`)

**Experimental.** Manage cloud-based tasks.

```bash
savfox cloud exec "<query>" --env <ENV_ID>  # submit a task
savfox cloud status <TASK_ID>               # check status
savfox cloud list                           # list tasks
savfox cloud apply <TASK_ID>                # apply diff locally
savfox cloud diff <TASK_ID>                 # show diff
```

### `completion`

Generate shell completion scripts.

```bash
savfox completion bash > ~/.bash_completion.d/savfox
savfox completion zsh > ~/.zfunc/_savfox
savfox completion fish > ~/.config/fish/completions/savfox.fish
savfox completion powershell > savfox.ps1
```

### `acp`

Run ACP bridge over stdio, backed by gateway WS-RPC.

```bash
savfox acp --gateway-url http://127.0.0.1:18881 --token "$SAVFOX_TOKEN"
```

See `docs/en/cli/acp-zed.md` for a full Zed configuration example.
