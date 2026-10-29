#!/bin/bash
# Benchmark RPC latency against a local gateway instance.
#
# Runs only the ignored `rpc_latency_benchmark` test from
# `crates/gateway-server/tests/e2e_gateway.rs`.

set -euo pipefail

TEST_TOKEN="bench-token-$(date +%s)"
TEST_PORT="${1:-18809}"
TEST_PORT="${E2E_PORT:-$TEST_PORT}"
TEST_HOME="${SAVFOX_HOME:-.e2e-savfox-home}"

echo "============================================"
echo " Savfox RPC Benchmark Runner"
echo "============================================"
echo "  Port:  $TEST_PORT"
echo "  Home:  $TEST_HOME"
echo ""

mkdir -p "$TEST_HOME"
export SAVFOX_HOME="$TEST_HOME"

echo "Building gateway..."
cargo build --bin savfox 2>&1

echo "Starting gateway on port $TEST_PORT..."
cargo run --bin savfox -- gateway --port "$TEST_PORT" --token "$TEST_TOKEN" &
GATEWAY_PID=$!

cleanup() {
    echo ""
    echo "Stopping gateway (PID $GATEWAY_PID)..."
    kill "$GATEWAY_PID" 2>/dev/null || true
    wait "$GATEWAY_PID" 2>/dev/null || true
    echo "Gateway stopped."
}
trap cleanup EXIT

echo "Waiting for gateway to be ready..."
READY=0
for i in $(seq 1 60); do
    if curl -sf "http://localhost:$TEST_PORT/health" >/dev/null 2>&1; then
        READY=1
        break
    fi
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

export E2E_GATEWAY_URL="http://localhost:$TEST_PORT"
export E2E_GATEWAY_TOKEN="$TEST_TOKEN"
export SAVFOX_RPC_BENCH_OUTPUT="${SAVFOX_RPC_BENCH_OUTPUT:-$TEST_HOME/rpc-benchmark.json}"

echo "Running RPC latency benchmark..."
echo "--------------------------------------------"
# Rust's test harness exits successfully when a filter matches zero tests.
# Verify the exact test exists before accepting a successful benchmark run.
TEST_LIST=$(cargo test -p savfox-gateway-server --test e2e_gateway -- --list)
if ! printf '%s\n' "$TEST_LIST" | grep -qx 'rpc_latency_benchmark: test'; then
    echo "ERROR: rpc_latency_benchmark is missing; no benchmark was run."
    exit 1
fi
cargo test -p savfox-gateway-server --test e2e_gateway rpc_latency_benchmark -- --exact --ignored --nocapture
echo "--------------------------------------------"
echo "RPC benchmark completed. Report: $SAVFOX_RPC_BENCH_OUTPUT"
