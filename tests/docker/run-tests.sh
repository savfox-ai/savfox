#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run-tests.sh -- E2E smoke tests for the savfox gateway
# ---------------------------------------------------------------------------
# This script is executed by the test-runner container in docker-compose.
# It verifies core HTTP endpoints and basic WebSocket JSON-RPC via curl.
# ---------------------------------------------------------------------------
set -euo pipefail

# If running inside docker-compose the env vars are set by the compose file.
# When running standalone, fall back to localhost.
GATEWAY_URL="${GATEWAY_URL:-http://localhost:18881}"
WS_URL="${WS_URL:-ws://localhost:18881/ws}"
TEST_TOKEN="${TEST_TOKEN:-e2e-test-token-00000000}"

PASS=0
FAIL=0

# Install curl and jq if missing (test-runner container is bare debian).
if ! command -v curl &>/dev/null || ! command -v jq &>/dev/null; then
    apt-get update -qq && apt-get install -y -qq curl jq >/dev/null 2>&1
fi

# ── Helpers ────────────────────────────────────────────────────────────────

pass() {
    PASS=$((PASS + 1))
    echo "[PASS] $1"
}

fail() {
    FAIL=$((FAIL + 1))
    echo "[FAIL] $1: $2" >&2
}

assert_status() {
    local desc="$1" url="$2" expected="$3"
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" "$url") || true
    if [ "$status" = "$expected" ]; then
        pass "$desc (HTTP $status)"
    else
        fail "$desc" "expected HTTP $expected, got $status"
    fi
}

assert_json_field() {
    local desc="$1" url="$2" field="$3" expected="$4"
    local body value
    body=$(curl -sf "$url") || { fail "$desc" "curl failed"; return; }
    value=$(echo "$body" | jq -r "$field") || { fail "$desc" "jq failed"; return; }
    if [ "$value" = "$expected" ]; then
        pass "$desc ($field=$value)"
    else
        fail "$desc" "expected $field=$expected, got $value"
    fi
}

# ── Test Suite ─────────────────────────────────────────────────────────────

echo ""
echo "========================================"
echo "  Savfox Gateway E2E Tests"
echo "  Target: $GATEWAY_URL"
echo "========================================"
echo ""

# 1. Health endpoint
echo "--- HTTP Endpoints ---"
assert_status "GET /health returns 200" "$GATEWAY_URL/health" "200"

# 2. Status endpoint
assert_status "GET /api/status returns 200" "$GATEWAY_URL/api/status" "200"

# 3. Status payload contains expected fields
assert_json_field "Status has version field" "$GATEWAY_URL/api/status" ".version" "$(
    curl -sf "$GATEWAY_URL/api/status" | jq -r '.version'
)"

# 4. Token validation -- valid token
echo ""
echo "--- Token Validation ---"
VALIDATE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer $TEST_TOKEN" \
    "$GATEWAY_URL/api/token/validate") || true
if [ "$VALIDATE_STATUS" = "200" ]; then
    pass "Token validation with valid token (HTTP 200)"
else
    fail "Token validation with valid token" "expected 200, got $VALIDATE_STATUS"
fi

# 5. Token validation -- invalid token
VALIDATE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer bad-token-value" \
    "$GATEWAY_URL/api/token/validate") || true
if [ "$VALIDATE_STATUS" = "401" ] || [ "$VALIDATE_STATUS" = "403" ]; then
    pass "Token validation rejects invalid token (HTTP $VALIDATE_STATUS)"
else
    fail "Token validation rejects invalid token" "expected 401 or 403, got $VALIDATE_STATUS"
fi

# 6. Unknown route returns 404
echo ""
echo "--- Error Handling ---"
assert_status "GET /nonexistent returns 404" "$GATEWAY_URL/nonexistent" "404"

# 7. WebSocket endpoint upgrade (without upgrade headers returns 400 or similar)
echo ""
echo "--- WebSocket Smoke ---"
WS_HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" "$GATEWAY_URL/ws") || true
if [ "$WS_HTTP_STATUS" = "400" ] || [ "$WS_HTTP_STATUS" = "426" ] || [ "$WS_HTTP_STATUS" = "405" ]; then
    pass "GET /ws without Upgrade header rejected (HTTP $WS_HTTP_STATUS)"
else
    fail "GET /ws rejection" "expected 400/405/426, got $WS_HTTP_STATUS"
fi

# ── Summary ────────────────────────────────────────────────────────────────

echo ""
echo "========================================"
echo "  Results: $PASS passed, $FAIL failed"
echo "========================================"
echo ""

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi

exit 0
