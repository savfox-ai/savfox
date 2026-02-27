#!/bin/bash
# Start gateway in development mode with auto-reload
# Usage: ./scripts/dev-gateway.sh [--port 18881] [--token mytoken]
set -euo pipefail

PORT="${1:-18881}"
TOKEN="${2:-dev-token-$(date +%s)}"

echo "Starting Savfox Gateway in development mode..."
echo "  Port: $PORT"
echo "  Token: $TOKEN"
echo "  Press Ctrl+C to stop"
echo ""

# Use cargo-watch for auto-reload if available
if command -v cargo-watch &>/dev/null; then
    cargo watch -x "run --bin savfox -- gateway --port $PORT --token $TOKEN"
elif command -v watchexec &>/dev/null; then
    watchexec -r -w crates/ -- cargo run --bin savfox -- gateway --port "$PORT" --token "$TOKEN"
else
    echo "Note: Install cargo-watch or watchexec for auto-reload"
    echo "  cargo install cargo-watch"
    echo ""
    cargo run --bin savfox -- gateway --port "$PORT" --token "$TOKEN"
fi
