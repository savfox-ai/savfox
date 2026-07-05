# Terminal Agent Release Checklist

Use this before promoting Terminal Agent Runtime changes from experimental to
release-ready. The checklist below reflects the current productionized
terminal-agent slice.

## Configuration

- [x] Agent configs use a required `kind` discriminator with either `native` or
  `terminal` branch data.
- [x] Terminal agents require `terminal.runtime = "codex" | "claude"`,
  `enabled = true`, and a non-empty command.
- [x] Native agents require `native.provider` and `native.model`.
- [x] `health_check_command` / `health_check_args` are optional and do not make
  existing agents fail health checks when omitted.

## Permissions

- [x] UI and docs state that native terminal delegates run vendor CLIs directly.
- [x] `savfox_approval_bridge = "prompt" | "required"` is documented as a
  declaration until the runtime reports bridge support.
- [x] Privileged RPC methods that spawn terminals or delete local terminal
  session data require Admin scope.

## Workspace

- [x] `shared` keeps the previous cwd behavior.
- [x] `worktree` creates an isolated detached git worktree per session.
- [x] `patch_only` writes `workspace.patch` and `workspace-diff-summary.txt`.
- [x] `read_only` either uses a real platform read-only sandbox or returns a
  clear unsupported-capability error.
- [x] Cleanup status, workspace paths, patch path, and diff summary path are
  recorded in metadata.

## Runtime And Observability

- [x] One-shot and interactive launch use the same command/cwd/env resolver.
- [x] Terminal stream order remains `started` -> logs/status/errors -> message
  -> completed/error.
- [x] `agent.terminal.health` reports command availability, cwd state, and
  terminal runtime root writability.
- [x] `agent.terminal.metrics` reports spawn count, durations, timeouts, and
  exit-reason totals.
- [x] `agent.terminal.cleanup` supports `dry_run` and only targets paths under
  `{savfox_home}/terminal-agents`.

## Managed PTY

- [x] Public WS-RPC methods expose managed PTY start/write/read/resize/close,
  list, and idle cleanup.
- [x] Process-backed fake REPL tests cover create/reuse/write/read/close.
- [x] Platform-specific PTY support is behind the `TerminalPtyBackend` trait.
- [x] Reconnect metadata clearly distinguishes attached, closed, and
  manual-rebind-needed states.

## Verification

- [x] `cargo test -p savfox-gateway-shared`
- [x] `cargo test -p savfox-gateway-server terminal_agent`
- [x] `cargo test -p savfox-gateway-server agent_terminal_delegate`
- [x] `cargo test -p savfox-gateway-server terminal_pty`
- [x] `cargo test -p savfox-gateway-dioxus terminal`
- [x] `cargo check -p savfox-gateway-server --lib`
- [x] `cargo check -p savfox-gateway-dioxus`
- [x] `cargo fmt -p savfox-gateway-server -p savfox-gateway-dioxus -p savfox-gateway-shared -- --check`
- [x] `git diff --check`
