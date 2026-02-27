# System Architecture

Savfox is a modular AI coding agent built as a Rust workspace with 50+ crates. This document describes the high-level architecture and how the major subsystems fit together.

## Overview

```
                    CLI / TUI
                       |
                  +----+----+
                  |  Core   |  (ThreadManager, Config, Tools, Sandbox)
                  +----+----+
                       |
              +--------+--------+
              |                 |
        LLM Providers     Gateway Server
        (codex-api)       (gateway-server)
              |                 |
     OpenAI / Anthropic   +----+----+
     Ollama / Groq / ...  | Bridges |
                          +---------+
                          Discord, Telegram,
                          Slack, Matrix, ...
```

## Core Engine (`crates/core`)

The core crate is the heart of Savfox. It contains:

- **ThreadManager** -- Manages conversation threads, message history, and context compaction. Each thread holds the full message sequence sent to the LLM.
- **Config** -- Layered configuration system. Merges system defaults, user config (`config.toml`), workspace config, profiles, CLI overrides, and cloud requirements.
- **AuthManager** -- Handles credential storage (system keyring), token refresh, and API key injection.
- **Tool System** -- Defines available tools (shell, file edit, patch, search, memory, etc.) and dispatches tool calls from the LLM to handler functions.
- **Sandbox** -- Enforces execution policies (read-only, workspace-write, full-access) and delegates to platform-specific sandbox implementations.

## LLM Provider Layer (`crates/codex-api`)

The `codex-api` crate abstracts over multiple LLM providers with a unified streaming interface:

- **WireApi** -- Four wire formats: `Responses`, `Chat`, `Compact`, `Anthropic`.
- **Request builders** -- Construct provider-specific HTTP requests with proper auth headers, model parameters, and tool definitions.
- **SSE parsers** -- Parse streaming responses from each wire format into a common event stream.
- **Retry** -- Automatic retry with exponential backoff for rate limits (429), server errors (5xx), and transport failures.

New OpenAI-compatible providers need zero code -- just register them with `WireApi::Chat` and a base URL.

## Gateway Server (`crates/gateway-server`)

The gateway exposes Savfox over HTTP and WebSocket for remote access:

- **REST API** -- Health checks, status, config management, session listing, agent invocation, OpenAI-compatible `/v1/chat/completions`, and execution approvals.
- **WebSocket JSON-RPC** -- Real-time bidirectional protocol with 90+ methods for agent control, session management, cron scheduling, and more.
- **Session Manager** -- Tracks active sessions per client, with file-based persistence and TTL-based pruning.
- **Cron Service** -- Timer-based scheduler for recurring agent tasks (daily reports, periodic checks).
- **Rate Limiter** -- Token-bucket rate limiting per authentication token.

Built on **Salvo** (v0.89), a Rust async HTTP framework.

## Chat Bridges

Bridges connect external messaging platforms to the gateway. Each bridge:

1. Receives messages from the platform (via webhook or polling).
2. Routes them to the session manager, which finds or creates a session.
3. Forwards the message to the core agent.
4. Sends the agent's response back to the platform.

Supported bridges: Discord, Telegram, Slack, Matrix, Mattermost, Google Chat, Line, Feishu, IRC, Signal, WhatsApp, Webhook.

## Tool System

The agent interacts with the environment through tools:

| Tool | Purpose |
|------|---------|
| `shell` | Execute shell commands |
| `apply_patch` | Apply unified diffs to files |
| `file_search` | Search files by name or content |
| `md_memory` | Read/write the 4-layer memory system |
| `browser` | Browser automation |
| `mcp` | Call MCP (Model Context Protocol) servers |

Tools are defined as JSON schemas sent to the LLM. When the model calls a tool, the core engine dispatches to the appropriate handler, sandboxes the execution, and returns the result.

## Sandbox Layer

Command execution is sandboxed at the OS level:

- **Linux**: Landlock LSM for filesystem restrictions + seccomp for syscall filtering. Runs via the `savfox-linux-sandbox` helper binary.
- **macOS**: Apple Seatbelt (`sandbox-exec`) with dynamically generated `.sbpl` policy files.
- **Windows**: Restricted process tokens with reduced privileges via `CreateProcessAsUserW`.

## App Server Protocol (`crates/app-server-protocol`)

A JSON-RPC protocol over stdio for IDE extensions. Defines `ClientRequest` and `ServerNotification` enums with TypeScript and JSON Schema code generation. This is the protocol used by VS Code and other editor integrations.

## Memory System

A 4-layer Markdown memory system provides persistent context across sessions. See [Memory](memory.md) for details.

## Data Flow

1. User sends a message (CLI, TUI, gateway WebSocket, or chat bridge).
2. The message reaches the core engine's ThreadManager.
3. ThreadManager builds the LLM request with system prompt, memory context, and message history.
4. The request streams to the configured LLM provider.
5. The LLM responds with text and/or tool calls.
6. Tool calls are dispatched to handlers, sandboxed, and results are fed back.
7. The cycle repeats until the agent completes its task.
8. The final response is returned to the user.
