# Getting Started

## Prerequisites

- **Rust 1.98+** (if building from source)
- A supported LLM provider account (OpenAI, or a local provider like Ollama / LM Studio)
- Git (for session diffs and patch application)

## Installation

### From Source

```bash
git clone https://github.com/savfox-ai/savfox.git
cd savfox
cargo install --path crates/savfox-cli
```

### Verify Installation

```bash
savfox --version
```

## Authentication

Before using Savfox, authenticate with your LLM provider:

```bash
savfox login
```

This opens an interactive flow to configure your API credentials. Savfox stores credentials securely in your system keyring.

To use an open-source local provider instead:

```bash
savfox --oss login
```

To log out:

```bash
savfox logout
```

## Your First Interactive Session

Simply run `savfox` with no arguments to launch the interactive terminal UI:

```bash
savfox
```

This opens a TUI (terminal user interface) where you can:
- Type messages and chat with the AI agent
- Review proposed file changes with diffs
- Approve or reject commands and patches
- Switch models and personalities

See [Interactive Mode](interactive-mode.md) for details.

## Your First Non-Interactive Execution

Use the `exec` subcommand (alias `e`) to run a one-shot task:

```bash
savfox exec "Add error handling to src/main.rs"
```

The agent will process your request, propose changes, and output results directly to the terminal. For JSON output (useful for scripting):

```bash
savfox exec --json "List all TODO comments in the codebase"
```

See [CLI Reference](cli-reference.md) for all available commands and options.

## Quick Tips

- Use `--model` or `-m` to specify a different LLM model:
  ```bash
  savfox -m gpt-4o exec "Explain this code"
  ```

- Use `--full-auto` for low-friction automated execution:
  ```bash
  savfox --full-auto exec "Refactor the auth module"
  ```

- Resume a previous session:
  ```bash
  savfox resume        # interactive picker
  savfox resume --last # resume the most recent session
  ```

## Next Steps

- [Interactive Mode](interactive-mode.md) — Learn the TUI features
- [CLI Reference](cli-reference.md) — All commands and flags
- [Configuration](configuration.md) — Customize Savfox behavior
- [Gateway](gateway.md) — Remote access and chat bridges
