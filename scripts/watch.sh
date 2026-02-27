#!/bin/bash
# Watch for changes and rebuild
# Usage: ./scripts/watch.sh [crate-name]
set -euo pipefail

CRATE="${1:-}"

if command -v cargo-watch &>/dev/null; then
    if [ -n "$CRATE" ]; then
        echo "Watching crate: $CRATE"
        cargo watch -x "check -p $CRATE"
    else
        echo "Watching entire workspace"
        cargo watch -x "check --workspace"
    fi
else
    echo "cargo-watch not installed. Install with:"
    echo "  cargo install cargo-watch"
    exit 1
fi
