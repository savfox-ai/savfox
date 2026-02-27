#!/bin/bash
# Build documentation site using mdBook.
# Usage: ./scripts/docs-build.sh [--serve]
set -euo pipefail

SERVE=false
if [[ "${1:-}" == "--serve" ]]; then
    SERVE=true
fi

# Check for mdbook
if ! command -v mdbook &>/dev/null; then
    echo "mdbook not installed. Install with:"
    echo "  cargo install mdbook"
    exit 1
fi

# Generate book.toml if missing
if [[ ! -f docs/book.toml ]]; then
    cat > docs/book.toml <<'TOML'
[book]
title = "Savfox Documentation"
authors = ["Savfox Contributors"]
language = "en"
multilingual = false
src = "en"

[output.html]
default-theme = "navy"
preferred-dark-theme = "navy"
git-repository-url = "https://github.com/savfox-ai/savfox"
edit-url-template = "https://github.com/savfox-ai/savfox/edit/main/docs/{path}"

[output.html.search]
enable = true
TOML
    echo "Generated docs/book.toml"
fi

# Generate SUMMARY.md from directory structure
SUMMARY="docs/en/SUMMARY.md"
cat > "$SUMMARY" <<'EOF'
# Summary

[Getting Started](getting-started.md)
[Interactive Mode](interactive-mode.md)
[Configuration](configuration.md)

# CLI Reference

- [CLI Reference](cli-reference.md)
- [Exec (Non-Interactive)](cli/exec.md)
- [Gateway](cli/gateway.md)

# Channels

- [Discord](channels/discord.md)
- [Telegram](channels/telegram.md)
- [Slack](channels/slack.md)

# LLM Providers

- [OpenAI](providers/openai.md)
- [Anthropic](providers/anthropic.md)
- [Ollama](providers/ollama.md)
- [Custom Providers](providers/custom.md)

# Concepts

- [Architecture](concepts/architecture.md)
- [Sessions](concepts/sessions.md)
- [Memory System](concepts/memory.md)
- [Message Routing](concepts/routing.md)

# Gateway

- [WebSocket Protocol](gateway/protocol.md)
- [REST API](gateway/api.md)
- [Gateway Guide](gateway.md)

# Security

- [Sandbox](security/sandbox.md)

# MCP

- [MCP Server](mcp-server.md)
EOF
echo "Generated $SUMMARY"

cd docs

if [[ "$SERVE" == "true" ]]; then
    echo "Starting documentation server..."
    mdbook serve --open
else
    echo "Building documentation..."
    mdbook build
    echo "Documentation built at docs/book/"
fi
