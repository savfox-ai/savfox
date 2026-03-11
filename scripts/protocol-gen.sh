#!/bin/bash
# Generate Gateway protocol JSON schema from Rust types
# Usage: ./scripts/protocol-gen.sh
set -euo pipefail

echo "Generating Gateway protocol schema..."

# Build and run the schema generator
cargo run --bin savfox -- features list 2>/dev/null || true

echo "Note: Full protocol schema generation requires a dedicated binary."
echo "Consider adding a build.rs or separate tool that uses schemars to"
echo "generate JSON schemas from GatewayMessage and related types."
echo ""
echo "Manual schema location: docs/en/gateway/protocol.md"
