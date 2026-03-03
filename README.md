# Savfox

[![CI](https://github.com/savfox-ai/savfox/workflows/CI/badge.svg)](https://github.com/savfox-ai/savfox/actions)
[![codecov](https://codecov.io/gh/savfox-ai/savfox/graph/badge.svg)](https://codecov.io/gh/savfox-ai/savfox)

Savfox is an AI assistant. It connects to LLM providers (OpenAI, Ollama, LM Studio, and more) to help you write, review, and refactor code through an interactive TUI or non-interactive CLI.

[English Documentation](docs/en/getting-started.md) | [中文文档](docs/zh/getting-started.md)

## Features

- **Interactive TUI** — Chat with an AI agent in a rich terminal interface with markdown rendering, diff preview, and approval workflows
- **Non-interactive CLI** — Run one-shot tasks with `savfox exec`, pipe results, and integrate into scripts
- **Gateway Server** — Remote HTTP/WebSocket access with OpenAI-compatible API, session management, and cron scheduling
- **Chat Bridges** — Connect to Discord, Telegram, Slack, Matrix, Mattermost, Google Chat, Line, Feishu, IRC, and Webhook
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

> **How it works:** `build.rs` in `gateway-server` declares the `dist/` folder as
> a Cargo dependency, so the backend automatically re-embeds the fresh frontend after
> each `dx build --web` — no manual cache-busting needed.

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

### 中文

| 指南 | 说明 |
| ---- | ---- |
| [快速开始](docs/zh/getting-started.md) | 安装、登录、首次运行 |
| [交互模式](docs/zh/interactive-mode.md) | TUI 功能、会话、审批 |
| [CLI 参考](docs/zh/cli-reference.md) | 所有命令、选项和示例 |
| [网关服务器](docs/zh/gateway.md) | 远程访问、API、聊天桥接 |
| [配置](docs/zh/configuration.md) | 配置文件、档案、特性标志 |
| [沙箱与安全](docs/zh/sandbox.md) | 沙箱模式、平台安全 |
| [MCP 服务器](docs/zh/mcp-server.md) | MCP 集成与使用 |

## License

Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
