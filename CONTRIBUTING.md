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
├── savfox-cli/          # Main binary (CLI dispatcher)
├── core/                # CodeX engine, config, tools, memory
├── gateway-server/      # HTTP/WebSocket gateway + Web UI
├── codex-api/           # Multi-provider LLM client
├── app-server/          # JSON-RPC for IDE extensions
├── plugin-sdk/          # Plugin development SDK
├── skill-registry/      # Skill management
├── protocol/            # Shared protocol types
└── ...                  # 50+ additional crates
```

### Key Areas

| Area | Crate | Description |
|------|-------|-------------|
| CLI | `savfox-cli` | Command-line interface and dispatch |
| Core Engine | `core` | Agent loop, tools, config, memory |
| Gateway | `gateway-server` | WebSocket server, bridges, web UI |
| API Client | `codex-api` | LLM provider integrations |
| Plugins | `plugin-sdk` | Extension/plugin system |

## Reporting Issues

- **Bugs**: Use the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md)
- **Features**: Use the [feature request template](.github/ISSUE_TEMPLATE/feature_request.md)
- **Questions**: Open a [Discussion](https://github.com/chrislearn/savfox/discussions)

## License

By contributing to Savfox, you agree that your contributions will be licensed under the MIT OR Apache-2.0 license.
