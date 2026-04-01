# Savfox

[![CI](https://github.com/savfox-ai/savfox/workflows/CI/badge.svg)](https://github.com/savfox-ai/savfox/actions)
[![codecov](https://codecov.io/gh/savfox-ai/savfox/graph/badge.svg)](https://codecov.io/gh/savfox-ai/savfox)

Savfox is a Rust-based AI coding assistant forked from [Codex](https://github.com/openai/codex). It connects to LLM providers (OpenAI, Ollama, LM Studio, and more) to help you write, review, and refactor code through an interactive TUI or non-interactive CLI. Savfox extends the original with a full gateway server layer — providing remote HTTP/WebSocket access, session management, cron scheduling, and multi-channel chat integration (Discord, Telegram, Slack, and more).

[Documentation](docs/en/getting-started.md)

## Features

- **Interactive TUI** — Chat with an AI agent in a rich terminal interface with markdown rendering, diff preview, and approval workflows
- **Non-interactive CLI** — Run one-shot tasks with `savfox exec`, pipe results, and integrate into scripts
- **Gateway Server** — Remote HTTP/WebSocket access with OpenAI-compatible API, session management, and cron scheduling
- **Chat Channels** — Connect to Discord, Telegram, Slack, Matrix, Mattermost, Google Chat, Line, Feishu, IRC, and Webhook
- **Multi-layer Sandbox** — Platform-native sandboxing (macOS Seatbelt, Linux Landlock, Windows restricted token) plus configurable approval policies
- **MCP Support** — Run as an MCP server for integration with Claude Desktop and other MCP clients, or connect to external MCP servers as tools
- **Session Management** — Resume, fork, and archive conversations across sessions
- **Cloud Tasks** — (Experimental) Submit and manage tasks in the cloud

## Quick Start

### Install

```bash
git clone https://github.com/savfox-ai/savfox.git
cd savfox
cargo install --path crates/savfox-cli
```

### Login

```bash
savfox login
```

### Run

```bash
# Interactive mode
savfox

# Non-interactive
savfox exec "Add error handling to src/main.rs"

# With auto-approval and workspace sandbox
savfox --full-auto exec "Refactor the auth module"
```

### Gateway Development

Build the web frontend and start the gateway server with [just](https://github.com/casey/just):

```bash
# Build frontend + start gateway (debug, default port 18881)
just gateway

# Override defaults
just port=9000 token=mysecret gateway

# Release build
just gateway-release
```

`just gateway` reuses the last Dioxus web build when `crates/gateway-dioxus/`,
`crates/gateway-shared/`, and the relevant shared frontend inputs have not changed.

For live frontend hot-reload during UI development, run two terminals:

```bash
# Terminal 1 — watches and rebuilds WASM on every save
just web-serve

# Terminal 2 — runs the gateway backend
just gateway-skip-web
```

Other available recipes:

```bash
just check     # cargo check --workspace
just lint      # cargo clippy --workspace
just test      # cargo test --workspace
just fmt       # cargo fmt --all
just help      # list all recipes
```

> **How it works:** `build.rs` in `gateway-server` declares the `static/` folder as
> a Cargo dependency, so the backend automatically re-embeds the fresh frontend after
> each synced `dx build --web`. `scripts/build-web.ps1` skips the web rebuild and static
> copy when the tracked frontend inputs have not changed.

## Documentation

### English

| Guide | Description |
| ----- | ----------- |
| [Getting Started](docs/en/getting-started.md) | Installation, login, first run |
| [Interactive Mode](docs/en/interactive-mode.md) | TUI features, sessions, approvals |
| [CLI Reference](docs/en/cli-reference.md) | All commands, flags, and examples |
| [Gateway Server](docs/en/gateway.md) | Remote access, API, chat bridges |
| [Configuration](docs/en/configuration.md) | Config file, profiles, feature flags |
| [Sandbox & Security](docs/en/sandbox.md) | Sandbox modes, platform security |
| [MCP Server](docs/en/mcp-server.md) | MCP integration and usage |


## License

Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
