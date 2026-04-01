#!/bin/bash
# Run tests in parallel across workspace crates
# Usage: ./scripts/test-parallel.sh
set -euo pipefail

echo "Running workspace tests in parallel..."
echo ""

# Use cargo-nextest if available (parallel by default)
if command -v cargo-nextest &>/dev/null; then
    cargo nextest run --workspace
else
    # Fallback to standard cargo test with threading
    cargo test --workspace --jobs "$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"
fi

echo ""
echo "All tests passed."
