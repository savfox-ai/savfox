# savfox-cli

The primary CLI entrypoint for Savfox. This crate produces the `savfox` binary and acts as a multitool dispatcher, routing user invocations to the appropriate subsystem based on the subcommand provided. When no subcommand is given, it launches the interactive TUI session.

Supported subcommands include `exec` (non-interactive execution), `review` (code review), `login`/`logout` (credential management), `mcp` / `mcp-server` (MCP server management), `app-server` (app server and code generation tooling), `resume`/`fork` (session continuation), `sandbox` (run commands under platform-specific sandboxes -- Seatbelt on macOS, Landlock on Linux, restricted tokens on Windows), `apply` (apply agent diffs locally), `cloud` (cloud task management), `completion` (shell completion generation), and `features` (feature flag inspection and toggling).

The library portion (`savfox_cli`) exposes CLI argument types for the sandbox commands (`SeatbeltCommand`, `LandlockCommand`, `WindowsCommand`), the login helpers, and a debug sandbox runner. The `main.rs` handles argument parsing via `clap`, config override merging, feature toggle validation, and delegates to the respective crate entry points (`savfox-tui`, `savfox-exec`, `savfox-mcp-server`, `savfox-app-server`, etc.). It also manages post-exit output such as token usage summaries and self-update actions.
