# Command Execution

The exec tool allows agents to run shell commands in a controlled environment.

## Overview

Agents can execute commands to:
- Read and write files
- Run build tools and tests
- Query system information
- Interact with APIs via curl
- Manage git repositories

## Configuration

```toml
[tools.exec]
enabled = true
sandbox = "auto"  # auto, docker, seatbelt, landlock, none
timeout_secs = 120
working_dir = "."
```

## Sandbox Modes

| Mode | Platform | Description |
|------|----------|-------------|
| `auto` | All | Detect best available sandbox |
| `docker` | All | Run in Docker container |
| `seatbelt` | macOS | App Sandbox via `sandbox-exec` |
| `landlock` | Linux 5.13+ | Landlock LSM filesystem restrictions |
| `none` | All | No sandboxing (development only) |

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `enabled` | `true` | Enable command execution |
| `sandbox` | `"auto"` | Sandbox mode |
| `timeout_secs` | `120` | Command timeout |
| `working_dir` | `"."` | Default working directory |
| `allow_commands` | `[]` | Whitelist commands (empty = all) |
| `deny_commands` | `[]` | Blacklist commands |
| `max_output_bytes` | `1048576` | Max output size (1MB) |
| `env_passthrough` | `[]` | Environment variables to pass through |

## Safety

### Command Allow/Deny Lists

Restrict which commands the agent can run:

```toml
[tools.exec]
allow_commands = ["git", "cargo", "npm", "python3", "curl"]
deny_commands = ["rm -rf /", "sudo", "chmod 777"]
```

### Network Restrictions

In Docker sandbox mode, network access can be restricted:

```toml
[tools.exec.sandbox_config]
network = "none"  # none, host, bridge
```

### File System Restrictions

Limit filesystem access:

```toml
[tools.exec.sandbox_config]
writable_paths = ["/workspace", "/tmp"]
readable_paths = ["/usr", "/etc"]
```

## Non-Interactive Mode

The `savfox exec` CLI command runs a single prompt without interactive mode:

```bash
savfox exec "List all TODO comments in the codebase"
savfox exec --model gpt-4o "Fix the bug in src/main.rs"
savfox exec --sandbox none "Run the test suite"
```

## Troubleshooting

- **Command not found**: The command may not be in PATH inside the sandbox
- **Permission denied**: Check sandbox filesystem restrictions
- **Timeout**: Increase `timeout_secs` for long-running commands
- **Output truncated**: Increase `max_output_bytes`
