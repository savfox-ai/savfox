# Contributing to Savfox

Thank you for your interest in contributing to Savfox! This document provides guidelines and instructions for contributing.

## Getting Started

### Prerequisites

- **Rust 1.89+** (edition 2024)
- **Git**
- **Cargo** (comes with Rust)

### Setup

```bash
git clone https://github.com/chrislearn/savfox.git
cd savfox
cargo build
```

### Install Git Hooks

```bash
git config core.hooksPath git-hooks
```

## Development Workflow

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Build specific crate
cargo build -p savfox-gateway-server

# Build the web UI (requires dioxus-cli)
cd crates/gateway-dioxus && dx build --release
```

### Testing

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p savfox-core

# Run with logging
RUST_LOG=debug cargo test --workspace
```

### Linting

```bash
# Format check
cargo fmt --all -- --check

# Clippy lints
cargo clippy --workspace --all-targets -- -D warnings
```

## Code Style

### Conventions

- **Edition 2024** — use Rust 2024 idioms
- **Workspace lints** are enforced:
  - `missing_debug_implementations = "warn"`
  - `unreachable_pub = "deny"`
  - `unsafe_code = "deny"`
- Library crates: `#![deny(clippy::print_stdout, clippy::print_stderr)]`
- Dependencies use workspace versions: `{ workspace = true }`

### Commit Messages

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): description

Types: feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert
```

Examples:
- `feat(gateway): add Nostr bridge`
- `fix(core): resolve session pruning race condition`
- `docs: update gateway protocol specification`

## Pull Requests

### Before Submitting

1. Ensure `cargo fmt --all -- --check` passes
2. Ensure `cargo clippy --workspace --all-targets -- -D warnings` passes
3. Ensure `cargo test --workspace` passes
4. Write tests for new functionality

### PR Guidelines

- Keep PRs focused on a single change
- Include a clear description of what and why
- Reference related issues
- Add tests for bug fixes and new features

### AI-Assisted PRs

AI-assisted contributions are welcome. Please indicate:
- [ ] This PR was AI-assisted
- [ ] Testing level: untested / lightly tested / fully tested
- [ ] Tool used (optional)

## Project Structure

```
crates/
├── savfox-cli/             # Main binary (CLI dispatcher)
├── core/                   # Agent engine, config, tools, memory
├── api-client/             # Multi-provider LLM HTTP client (OpenAI / Anthropic / Responses)
├── gateway-server/         # HTTP/WebSocket gateway + REST API + Web UI host
├── gateway-shared/         # Wire types shared between gateway-server and the web UI
├── gateway-dioxus/         # Dioxus-based web UI
├── app-server/             # JSON-RPC daemon used by TUI / IDE plugins
├── app-server-protocol/    # JSON-RPC v1 wire protocol for the app-server
├── protocol/               # Agent ↔ engine protocol (events, sandbox policies)
├── channels/               # Chat-platform adapters (Slack, Discord, Telegram, …)
├── skill-registry/         # Skill management
├── tui/                    # Terminal UI (ratatui + crossterm)
├── network-proxy/          # HTTP / SOCKS5 proxy + policy decider for sandboxed agents
├── exec-policy/            # Allow / forbid / ask DSL for shell command execution
├── apply-patch/            # `apply_patch` tool implementation
├── keyring-store/          # OS-keyring credential storage
├── login-oauth/            # OAuth 2.0 / PKCE login flows
└── …                       # ~30 more (linux-sandbox, windows-sandbox, exec-server, mcp-server, …)
```

Run `ls crates/` for the current full list (48 crates as of this writing).

### Key areas

| Area | Crate | Description |
|------|-------|-------------|
| CLI | `savfox-cli` | Command-line interface and dispatch |
| Core engine | `core` | Agent loop, tools, config, memory |
| Gateway | `gateway-server` | WebSocket / REST server, channels, web UI host |
| API client | `api-client` | LLM provider integrations (OpenAI, Anthropic, Responses) |
| Channels | `channels` | Slack / Discord / Telegram / Matrix / etc. adapters |
| Sandbox | `linux-sandbox`, `windows-sandbox`, `exec-policy` | Per-platform sandboxes + the policy DSL |

### Adding a new chat channel

The minimal recipe (see [`docs/en/channels/adapter-contract.md`](docs/en/channels/adapter-contract.md) for the full trait surface):

1. Create `crates/channels/src/<platform>/{client.rs,parse.rs,config.rs,mod.rs}` following the existing modules.
2. Add the platform to `ChannelsConfig` in `crates/gateway-server/src/config/config.rs`.
3. Wire the webhook route in `crates/gateway-server/src/server.rs::build_router`.
4. Mirror the config struct in `crates/gateway-shared/src/channels.rs` so the web UI can render the settings page.
5. Add inbound + outbound parser tests; constant-time HMAC verify (see `crates/channels/src/http.rs::verify_webhook_hmac` for the pattern).

### Adding a new LLM provider

The minimal recipe (see [`docs/en/providers/custom.md`](docs/en/providers/custom.md) for the full reference):

1. Pick the wire dialect (`crates/api-client/src/requests/<dialect>.rs` already covers OpenAI Chat, OpenAI Responses, Anthropic Messages).
2. Register the provider id, base URL, retry config in `crates/core/src/providers/info.rs`.
3. If the streaming SSE shape differs from existing dialects, add a parser under `crates/api-client/src/sse/`.
4. Add at least one happy-path SSE fixture test (see `sse::anthropic::tests::parses_text_response` for the template).

## Reporting Issues

- **Bugs**: Use the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md)
- **Features**: Use the [feature request template](.github/ISSUE_TEMPLATE/feature_request.md)
- **Questions**: Open a [Discussion](https://github.com/chrislearn/savfox/discussions)

## License

By contributing to Savfox, you agree that your contributions will be dual-licensed under your choice of [`LICENSE-MIT`](LICENSE-MIT) or [`LICENSE-APACHE`](LICENSE-APACHE) — the same `MIT OR Apache-2.0` terms the workspace `Cargo.toml` advertises.
