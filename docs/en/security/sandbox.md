# Sandbox Design

Savfox sandboxes all agent-executed commands at the operating system level. This prevents a misbehaving or manipulated LLM from accessing files, networks, or system resources outside the defined policy.

## Sandbox Modes

Savfox provides three sandbox modes that control what the agent can do:

| Mode | File System | Network |
|------|-------------|---------|
| `read-only` | No writes anywhere | Allowed (for LLM API calls) |
| `workspace-write` | Write only within the workspace directory | Allowed |
| `full-access` | Unrestricted writes | Allowed |

Set the mode with `--sandbox`:

```bash
savfox --sandbox read-only exec "Analyze this codebase"
savfox --sandbox workspace-write exec "Fix the tests"
```

The `workspace-write` mode also blocks writes to `.git/` and `.savfox/` within the workspace to prevent accidental corruption of repository metadata.

## Additional Writable Directories

Grant write access to directories outside the workspace:

```bash
savfox --sandbox workspace-write --add-dir /tmp/output exec "Generate reports"
```

## Platform Implementations

### Linux: Landlock + seccomp

On Linux, Savfox uses two kernel security mechanisms:

**Landlock** (Linux 5.13+) is a Linux Security Module that restricts filesystem access at the process level. Savfox generates a Landlock ruleset from the `SandboxPolicy`:

- Read-only paths get `LANDLOCK_ACCESS_FS_READ_FILE` and `LANDLOCK_ACCESS_FS_READ_DIR`.
- Writable paths additionally get `LANDLOCK_ACCESS_FS_WRITE_FILE`, `LANDLOCK_ACCESS_FS_MAKE_REG`, etc.
- All other paths are inaccessible.

**seccomp** (secure computing mode) filters syscalls. Savfox blocks dangerous syscalls like `ptrace`, `mount`, and `reboot` while allowing the standard set needed for shell commands and file operations.

The sandbox runs via a dedicated helper binary (`savfox-linux-sandbox`) that:

1. Receives the sandbox policy as a JSON argument.
2. Installs the Landlock ruleset and seccomp filter.
3. Executes the target command under the restricted environment.

```bash
savfox sandbox linux -- ls /etc
```

### macOS: Seatbelt

On macOS, Savfox uses Apple's `sandbox-exec` with dynamically generated Scheme-based policy files (`.sbpl`).

The policy is built from a base template (`seatbelt_base_policy.sbpl`) that:

- Denies all file writes by default.
- Allows reads to system libraries and standard paths.
- Injects writable directory rules based on the `SandboxPolicy`.
- Optionally adds network access rules.

Seatbelt enforcement is done by `/usr/bin/sandbox-exec` (the system binary). Savfox deliberately only uses the system path to prevent PATH injection attacks.

```bash
savfox sandbox macos -- cat /etc/hosts
```

### Windows: Restricted Token

On Windows, Savfox creates a restricted process token using the Win32 API:

1. Calls `CreateRestrictedToken` to remove privileges from the current process token.
2. Disables SIDs (Security Identifiers) that grant write access outside the allowed paths.
3. Launches the child process with `CreateProcessAsUserW` using the restricted token.

The restricted token approach:

- Strips administrative privileges.
- Removes membership in power groups.
- Limits file system access to the workspace and system directories.

```bash
savfox sandbox windows -- dir C:\
```

## How It Works in Practice

When the agent calls the `shell` tool:

1. The core engine receives the command.
2. Based on the current `SandboxPolicy`, it selects the platform sandbox implementation.
3. The command is executed under the sandbox.
4. stdout and stderr are captured and returned to the agent.
5. If the command attempts a blocked operation (e.g., writing outside the workspace in `workspace-write` mode), the OS sandbox denies it and the command fails with a permission error.

## Defense in Depth

The sandbox is one layer in Savfox's security model:

1. **Approval policy** -- The agent asks for human approval before running commands (unless overridden).
2. **Sandbox enforcement** -- OS-level restrictions on what the process can do.
3. **Trusted commands** -- A configurable list of commands that skip approval.
4. **Network isolation** -- Future: restrict network access to specific hosts.

## Best Practices

- Use `workspace-write` as the default for coding tasks. It prevents writes outside the project while letting the agent edit source files.
- Use `read-only` for analysis-only tasks (code review, explanation, search).
- Reserve `full-access` for tasks that genuinely need it (installing dependencies, writing to `/tmp`).
- Never use `--yolo` in production or on machines with sensitive data.
- On Linux, ensure your kernel supports Landlock (5.13+). On older kernels, the sandbox falls back to a less restrictive mode.
