# savfox-exec

Non-interactive (headless) CLI for Savfox. This crate provides both a library (`savfox_exec`) and a binary (`savfox-exec`) that runs the Savfox agent without a TUI, suitable for scripting, CI pipelines, and automation workflows.

The binary accepts a prompt (as an argument or via stdin), configures the agent with the appropriate model, approval policy, and sandbox mode, then streams events to stdout. Two output modes are supported: a human-readable format with optional ANSI color and a `--json` mode that emits one JSON event per line (JSONL). The exec binary also supports subcommands for resuming previous sessions (`resume`) and performing code reviews (`review`).

When invoked with arg0 set to `savfox-linux-sandbox`, the same binary dispatches to the Linux sandbox subsystem instead, allowing a single binary to serve both roles. The crate handles configuration loading, OSS provider bootstrapping (Ollama, LM Studio), OpenTelemetry initialization, and Ctrl+C interrupt forwarding to the agent thread.
