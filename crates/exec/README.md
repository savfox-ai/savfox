# savfox-exec

Non-interactive (headless) CLI for Savfox. This crate provides both a library (`savfox_exec`) and a binary (`savfox-exec`) that runs the Savfox agent without a TUI, suitable for scripting, CI pipelines, and automation workflows.

The binary accepts a prompt (as an argument or via stdin), configures the agent with the appropriate model, approval policy, and sandbox mode, then streams events to stdout. Two output modes are supported: a human-readable format with optional ANSI color and a `--json` mode that emits one JSON event per line (JSONL). The exec binary also supports subcommands for resuming previous sessions (`resume`) and performing code reviews (`review`).

When invoked with arg0 set to `savfox-linux-sandbox`, the same binary dispatches to the Linux sandbox subsystem instead, allowing a single binary to serve both roles. The crate handles configuration loading, OSS provider bootstrapping (Ollama, LM Studio), OpenTelemetry initialization, and Ctrl+C interrupt forwarding to the agent thread.

Ctrl+C cancels execution, waits for session and subprocess cleanup, and exits with status `130`, including when the signal arrives before initial submission or after core becomes idle. JSONL mode emits a single `turn.failed` event with `error.message: "Turn interrupted"` when no terminal event has been delivered yet. Interrupting a sub-agent alone does not end the primary exec session.

`resume` requires an existing session ID/name or `--last`. A missing session, an empty `--last` selection, or a history lookup error fails without creating a new session. See [execution lifecycle](../../docs/en/cli/execution-lifecycle.md).
