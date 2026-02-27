# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.3.x   | :white_check_mark: |
| < 0.3   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in Savfox, please report it responsibly:

1. **Do NOT** open a public GitHub issue
2. Email: chris@acroidea.com
3. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

We will acknowledge receipt within 48 hours and aim to provide a fix within 7 days for critical issues.

## Security Model

### Gateway Server

- **Token-based authentication** with scoped permissions (Operator, Viewer, Chat)
- **Rate limiting** on authentication attempts (token-bucket algorithm)
- **Non-root execution** in Docker containers
- **TLS support** for encrypted connections

### Agent Execution

- **Sandboxed execution** via platform-native mechanisms:
  - Linux: Landlock + seccomp
  - macOS: Seatbelt (sandbox-exec)
  - Windows: Restricted token
- **Execution approval** workflow for sensitive commands
- **Tool policy** enforcement (allow/deny lists)

### Data Protection

- **Credential storage** via OS keyring (keyring-store crate)
- **No plaintext secrets** in configuration files
- **Session data** stored locally with file permissions

## Threat Model

### In Scope

- Remote code execution via gateway WebSocket
- Authentication bypass
- Privilege escalation in sandbox
- Information disclosure via API endpoints
- Denial of service via resource exhaustion

### Out of Scope

- Physical access to the host machine
- Social engineering of users
- Vulnerabilities in upstream LLM providers
- Vulnerabilities in third-party chat platforms (Discord, Slack, etc.)
