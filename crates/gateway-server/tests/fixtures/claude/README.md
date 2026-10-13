# Synthetic Claude stdio peer

`cli.sh` is an offline synthetic protocol peer, not a recording of a supported Claude CLI release. Its `synthetic-claude-2` version intentionally cannot stand in for a real compatibility matrix entry.

The control envelopes, permission decisions, finite task ledger and session-state behavior were checked against Anthropic's Python SDK at commit `b6e9d12fe1cc98dde988ab7b7713c1feeee50c6c` on 2026-10-11:

- https://github.com/anthropics/claude-agent-sdk-python/blob/b6e9d12fe1cc98dde988ab7b7713c1feeee50c6c/src/claude_agent_sdk/_internal/query.py
- https://code.claude.com/docs/en/agent-sdk/permissions
- https://code.claude.com/docs/en/agent-sdk/sessions

Tests use POSIX sh, sed and awk. Production does not use this script or require a shell bridge. They verify transport/routing boundaries without credentials, model traffic or billable work. No actual CLI compatibility is asserted.
