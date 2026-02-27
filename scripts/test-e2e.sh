#!/bin/bash
# End-to-end test runner for the Savfox Gateway Server.
#
# This script:
#   1. Builds and starts a gateway instance in the background with a test token.
#   2. Waits for the /health endpoint to become available.
#   3. Runs the E2E integration tests.
#   4. Kills the gateway on exit (via trap).
#
# Usage:
#   ./scripts/test-e2e.sh
#   ./scripts/test-e2e.sh --port 18800
#
set -euo pipefail

TEST_TOKEN="e2e-test-token-$(date +%s)"
TEST_PORT="${1:-18799}"
TEST_HOME="${SAVFOX_HOME:-.e2e-savfox-home}"

# Allow overriding via environment
TEST_PORT="${E2E_PORT:-$TEST_PORT}"

echo "============================================"
echo " Savfox Gateway E2E Test Runner"
echo "============================================"
echo "  Port:  $TEST_PORT"
echo "  Token: $TEST_TOKEN"
echo "  Home:  $TEST_HOME"
echo ""

mkdir -p "$TEST_HOME"
export SAVFOX_HOME="$TEST_HOME"

echo "Building gateway..."
cargo build --bin savfox 2>&1

echo "Starting gateway on port $TEST_PORT..."
cargo run --bin savfox -- gateway --port "$TEST_PORT" --token "$TEST_TOKEN" &
GATEWAY_PID=$!

# Ensure the gateway is killed when this script exits (success or failure).
cleanup() {
    echo ""
    echo "Stopping gateway (PID $GATEWAY_PID)..."
    kill "$GATEWAY_PID" 2>/dev/null || true
    wait "$GATEWAY_PID" 2>/dev/null || true
    echo "Gateway stopped."
}
trap cleanup EXIT

# Wait for the gateway health endpoint to become available.
echo "Waiting for gateway to be ready..."
READY=0
for i in $(seq 1 60); do
    if curl -sf "http://localhost:$TEST_PORT/health" >/dev/null 2>&1; then
        READY=1
        break
    fi
    # Check that the gateway process is still alive.
    if ! kill -0 "$GATEWAY_PID" 2>/dev/null; then
        echo "ERROR: Gateway process exited unexpectedly."
        exit 1
    fi
    sleep 1
done

if [ "$READY" -ne 1 ]; then
    echo "ERROR: Gateway did not become ready within 60 seconds."
    exit 1
fi

echo "Gateway is ready."
echo ""

# Run the E2E tests.
export E2E_GATEWAY_URL="http://localhost:$TEST_PORT"
export E2E_GATEWAY_TOKEN="$TEST_TOKEN"

echo "Running E2E tests..."
echo "--------------------------------------------"
set +e
cargo test -p savfox-gateway-server --test gateway_lifecycle_test gateway_startup_shutdown_lifecycle -- --ignored --nocapture
LIFECYCLE_EXIT=$?
cargo test -p savfox-gateway-server --test e2e_gateway -- --ignored --nocapture
E2E_EXIT=$?
set -e
if [ "$LIFECYCLE_EXIT" -ne 0 ] || [ "$E2E_EXIT" -ne 0 ]; then
    TEST_EXIT=1
else
    TEST_EXIT=0
fi
echo "--------------------------------------------"

if [ "$TEST_EXIT" -eq 0 ]; then
    echo "All E2E tests passed."
else
    echo "Some E2E tests failed (exit code $TEST_EXIT)."
fi

exit "$TEST_EXIT"
