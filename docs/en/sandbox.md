# Sandbox and Security

Savfox provides multi-layered sandboxing to control what the AI agent can do on your system. This ensures safe execution of agent-generated commands and file operations.

## Sandbox Modes

Set the sandbox mode with `--sandbox`:

```bash
savfox --sandbox read-only exec "Analyze this codebase"
savfox --sandbox workspace-write exec "Refactor the auth module"
savfox --sandbox full-access exec "Install dependencies and build"
```

| Mode | Description |
|------|-------------|
| `read-only` | No file system writes allowed |
| `workspace-write` | Write only to the workspace directory (`.git` and `.savfox` are read-only) |
| `full-access` | Unrestricted file system access |

## Approval Policies

Control when the agent asks for your approval before executing actions:

```bash
savfox --ask-for-approval unless-trusted exec "Do something"
```

| Policy | Description |
|--------|-------------|
| `unless-trusted` | Ask unless the command is in the trusted list (default) |
| `on-request` | Ask only when the agent explicitly requests approval |
| `on-failure` | Ask only when a command fails |
| `never` | Auto-approve all actions (use with sandbox restrictions) |

## Convenience Modes

### Full Auto

Combines a relaxed approval policy with workspace-write sandboxing:

```bash
savfox --full-auto exec "Refactor the database module"
```

Equivalent to `--ask-for-approval on-request --sandbox workspace-write`.

### Bypass Mode (Dangerous)

Skips all confirmations and sandboxing. **Use only in trusted, isolated environments:**

```bash
savfox --yolo exec "Do whatever is needed"
```

This flag is intentionally named to remind you of the risk.

## Platform-Specific Sandboxing

Savfox supports native OS sandbox mechanisms for stronger isolation:

### macOS — Seatbelt

Uses Apple's `sandbox-exec` to enforce sandboxing at the OS level:

```bash
savfox sandbox macos <command>
# alias:
savfox sandbox seatbelt <command>
```

### Linux — Landlock + seccomp

Uses Linux kernel security modules for fine-grained access control:

```bash
savfox sandbox linux <command>
# alias:
savfox sandbox landlock <command>
```

### Windows — Restricted Token

Uses Windows restricted tokens to limit process privileges:

```bash
savfox sandbox windows <command>
```

## Additional Writable Directories

When using `workspace-write` mode, you can grant write access to additional directories:

```bash
savfox --sandbox workspace-write --add-dir /tmp/output exec "Generate reports to /tmp/output"
```

## Best Practices

1. **Default to `workspace-write`** — It prevents accidental modifications outside your project while allowing the agent to work effectively.

2. **Use `read-only` for analysis** — When you only need the agent to read and explain code, use read-only mode for maximum safety.

3. **Combine `never` approval with sandbox** — If you want hands-free execution, pair `--ask-for-approval never` with a restrictive sandbox mode rather than using `--yolo`.

4. **Use platform sandbox for untrusted tasks** — The `sandbox` subcommand provides OS-level isolation that is stronger than the built-in policy checks.

5. **Review approvals in gateway mode** — When running the gateway server, use `savfox gateway approvals list` to review pending approvals from remote clients.
