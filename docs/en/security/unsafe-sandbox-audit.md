# Unsafe and Sandbox Audit Checklist

Savfox denies `unsafe_code` at the workspace level. Any exception is part of the execution security boundary and needs explicit review when it changes.

## Review Scope

Review this checklist for changes touching:

- `crates/linux-sandbox`: Landlock, seccomp, mount namespaces, user namespaces, and sandbox process setup.
- `crates/windows-sandbox`: restricted tokens, ACL changes, process creation, firewall rules, DPAPI, and setup orchestration.
- `crates/exec-policy`: policy loading and any Starlark/runtime integration guarded by `allow(unsafe_code)`.
- `crates/arg0`: process re-exec, argv/env manipulation, and platform entrypoint handling.
- `crates/exec-server/src/posix`: Unix socket FD passing, escalation client/server code, and raw descriptor ownership.
- `crates/utils/src/pty`: Unix process groups, Windows ConPTY, handle inheritance, and process termination.
- `crates/core/src/config_loader`, `crates/core/src/seatbelt`, `crates/core/src/auth`, and `crates/core/src/commands/shell_snapshot` when they add or move FFI, raw descriptors, or process-global state.

## Required Review Notes

Each new or changed unsafe boundary must document:

- why safe Rust or an existing wrapper is insufficient
- which invariants callers must uphold
- who owns resource lifetime, including file descriptors, handles, allocated buffers, and SIDs
- how errors are surfaced instead of panicking across user-facing paths
- which platform and privilege assumptions are required
- which tests or manual smoke checks cover the path

Do not add a crate-level `#![allow(unsafe_code)]` without a nearby module-level explanation and a test plan in the PR.

## Platform Smoke Tests

Use the narrowest applicable smoke test first, then broaden when behavior crosses a shared boundary.

| Area | Suggested check |
| ---- | --------------- |
| Linux sandbox | `cargo test -p savfox-linux-sandbox` on Linux |
| Windows sandbox | `cargo test -p savfox-windows-sandbox` on Windows |
| Exec policy | `cargo test -p savfox-exec-policy` |
| POSIX exec server | `cargo test -p savfox-exec-server` on Linux or macOS |
| PTY/process group changes | `cargo test -p savfox-utils pty` on the affected platform |
| Cross-surface sandbox behavior | `cargo test -p savfox-exec --test suite sandbox` |

If the relevant platform is unavailable locally, call that out in the PR and rely on the manual or scheduled cross-platform CI sweep.

## Unsupported Platforms

Unsupported architecture or OS paths should return explicit errors with enough context for the CLI, TUI, or gateway to explain the limitation. Avoid `unimplemented!`, `todo!`, or unchecked panics in sandbox setup paths.
