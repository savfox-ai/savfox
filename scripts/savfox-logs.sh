#!/usr/bin/env bash
# savfox-logs.sh — View and tail Savfox gateway logs.
#
# Usage:
#   ./scripts/savfox-logs.sh           # Show recent logs
#   ./scripts/savfox-logs.sh -f        # Follow (tail) logs
#   ./scripts/savfox-logs.sh --json    # Raw JSON output
#   ./scripts/savfox-logs.sh --level error  # Filter by level
#   ./scripts/savfox-logs.sh --since 1h     # Show logs from last hour
#
# Environment:
#   SAVFOX_HOME — Savfox home directory (default: ~/.savfox)

set -euo pipefail

SAVFOX_HOME="${SAVFOX_HOME:-$HOME/.savfox}"
LOG_DIR="${SAVFOX_HOME}/logs"
FOLLOW=false
JSON=false
LEVEL=""
SINCE=""
LINES=100

usage() {
    echo "Usage: savfox-logs [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -f, --follow       Follow log output (like tail -f)"
    echo "  -n, --lines N      Show last N lines (default: 100)"
    echo "  --json             Raw JSON output"
    echo "  --level LEVEL      Filter by level (error, warn, info, debug, trace)"
    echo "  --since DURATION   Show logs from duration ago (1h, 30m, 1d)"
    echo "  -h, --help         Show this help"
    exit 0
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        -f|--follow) FOLLOW=true; shift ;;
        -n|--lines) LINES="$2"; shift 2 ;;
        --json) JSON=true; shift ;;
        --level) LEVEL="$2"; shift 2 ;;
        --since) SINCE="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

# Find the most recent log file
find_log_file() {
    if [ -d "$LOG_DIR" ]; then
        ls -t "$LOG_DIR"/*.log 2>/dev/null | head -1
    else
        echo ""
    fi
}

# Convert duration string to seconds
duration_to_secs() {
    local dur="$1"
    case "$dur" in
        *d) echo $(( ${dur%d} * 86400 )) ;;
        *h) echo $(( ${dur%h} * 3600 )) ;;
        *m) echo $(( ${dur%m} * 60 )) ;;
        *s) echo "${dur%s}" ;;
        *)  echo "$dur" ;;
    esac
}

# Try gateway API first
try_api() {
    local gateway_url="http://localhost:18881"
    local token_file="${SAVFOX_HOME}/.gateway-token"

    if [ -f "$token_file" ]; then
        local token
        token=$(cat "$token_file")
        local resp
        resp=$(curl -sf "${gateway_url}/api/status" -H "Authorization: Bearer $token" 2>/dev/null || true)
        if [ -n "$resp" ]; then
            # Gateway is running, use the logs API
            local params="limit=${LINES}"
            [ -n "$LEVEL" ] && params="${params}&level=${LEVEL}"
            [ -n "$SINCE" ] && params="${params}&since=${SINCE}"

            if [ "$FOLLOW" = true ]; then
                # Stream logs via SSE
                curl -sfN "${gateway_url}/api/logs/stream?${params}" \
                    -H "Authorization: Bearer $token" 2>/dev/null
                return 0
            else
                curl -sf "${gateway_url}/api/logs?${params}" \
                    -H "Authorization: Bearer $token" 2>/dev/null
                return 0
            fi
        fi
    fi
    return 1
}

# Fall back to file-based log reading
read_files() {
    local log_file
    log_file=$(find_log_file)

    if [ -z "$log_file" ]; then
        echo "No log files found in ${LOG_DIR}"
        echo "Is the gateway running? Try: savfox gateway --port 18881"
        exit 1
    fi

    # Apply level filter
    local filter_cmd="cat"
    if [ -n "$LEVEL" ]; then
        local level_upper
        level_upper=$(echo "$LEVEL" | tr '[:lower:]' '[:upper:]')
        filter_cmd="grep -i \"${level_upper}\""
    fi

    if [ "$FOLLOW" = true ]; then
        tail -f "$log_file" | eval "$filter_cmd"
    else
        tail -n "$LINES" "$log_file" | eval "$filter_cmd"
    fi
}

# Main
if ! try_api; then
    read_files
fi
