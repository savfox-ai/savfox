# Threat Model

This document describes the security boundaries and threat model for Savfox.

## Trust Boundaries

### 1. User ↔ Gateway

The gateway accepts connections from operators, nodes, and chat bridges. Trust is established via token-based authentication with scoped permissions.

**Threats:**
- Token theft or leakage
- Unauthorized access to operator functions
- WebSocket connection hijacking

**Mitigations:**
- Token scoping (Operator, Viewer, Chat)
- Rate limiting per client
- HTTPS/WSS for transport encryption
- Token validation on every request

### 2. Gateway ↔ LLM Providers

The gateway sends prompts and receives completions from external LLM APIs.

**Threats:**
- API key exposure
- Prompt injection from user messages
- Data exfiltration via crafted prompts

**Mitigations:**
- API keys stored in environment variables, never in logs
- System prompt isolation
- Input sanitization before provider calls

### 3. Agent ↔ Sandbox

Agents can execute commands in a sandboxed environment.

**Threats:**
- Container escape
- Filesystem access beyond allowed paths
- Network access to internal services
- Resource exhaustion (CPU, memory, disk)

**Mitigations:**
- Sandbox execution (Docker/seatbelt/landlock)
- Configurable allow/deny lists for commands
- Resource limits (timeout, memory caps)
- Network restrictions in sandbox mode

### 4. Gateway ↔ Chat Bridges

Chat bridges connect to external platforms (Discord, Slack, Telegram, etc.).

**Threats:**
- Malicious messages from chat users
- Bot token compromise
- Webhook replay attacks

**Mitigations:**
- Auto-reply rules with permission gates
- DM policy enforcement (who can trigger the agent)
- Webhook signature verification
- Rate limiting per user/channel

## Data at Rest

| Data | Location | Protection |
|------|----------|------------|
| Config (API keys) | `~/.savfox/config.toml` | File permissions (0600) |
| Session transcripts | `~/.savfox/sessions/` | File permissions, auto-pruning |
| Memory entries | `~/.savfox/memory/` | File permissions |
| Cron history | `~/.savfox/cron/` | File permissions |
| Audit logs | `~/.savfox/audit/` | Append-only |

## Secrets Management

- API keys should be set via environment variables, not config files
- The `.detect-secrets` baseline helps prevent accidental commits
- `savfox doctor` checks for common security misconfigurations
- Secrets are never logged or included in error messages

## Reporting Vulnerabilities

See [SECURITY.md](../../SECURITY.md) for the responsible disclosure process.
