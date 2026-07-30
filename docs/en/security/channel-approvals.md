# Channel execution security

Savfox resolves the same Agent security policy for WebSocket, HTTP, and Channel
entry points. Capability and interaction are separate:

- `permission_policy` controls the sandbox, tools, named profile, granular
  approval categories, and optional network intent. When `profile` is set, it
  is authoritative; the legacy `sandbox` field is used only when no profile is
  present.
- `execution_policy.mode` controls how a boundary request is handled:
  `interactive`, `unattended`, or `auto-review`.

`interactive` is used only when the entry point can return a correlated
decision. `unattended` never waits for a person: Core receives an immediate
denial. `auto-review` currently fails closed to unattended when no reviewer is
configured.

## Agent example

```json
{
  "permission_policy": {
    "profile": ":workspace",
    "approval": "granular",
    "granular_approval": {
      "sandbox_approval": true,
      "rules": true
    }
  },
  "execution_policy": {
    "mode": "interactive"
  }
}
```

Built-in profile names are `:read-only`, `:workspace`, and
`:danger-full-access`. Unknown profiles and malformed policies resolve
fail-closed.

## Correlated approvals

Every approval has a server-issued request ID, a single-use nonce, the exact
Core session/approval operation, Agent, Channel instance/account/peer, logical session,
policy fingerprint, expiry, and a redacted summary. Telegram and Discord can
render structured actions. Text clients can reply with:

```text
approve:<request-id>
approve-session:<request-id>
allow-rule:<request-id>
deny:<request-id>
abort:<request-id>
```

Bare `+` and `-` are compatibility aliases only when exactly one pending
request belongs to the same authenticated Channel scope. Timeout, expiry,
restart, replay, a different peer, or a different session cannot approve the
request.

Approval-request, approval-list, and approval-resolution tokens are distinct:
`operator_approvals_request` can create a request but cannot discover its nonce
or resolve it;
`operator_approvals_read` can discover pending nonces;
`operator_approvals_resolve` can submit a known request and nonce. The legacy
`operator_approvals` scope implies all three.

## Rules and simulation

Gateway approval-policy writes are deprecated. Existing global rules are
strictly parsed once and migrated to `rules/default.rules`; per-node rules are
not widened to global scope. New permanent decisions use Core execpolicy only.

The Agent page exposes the effective enforcement backend, Core rules, and a
policy simulator. The simulator calls `security.policy.simulate` and evaluates
the real layered Core policy without executing the command. Rule changes use
`security.rules.add` and `security.rules.remove`.

## Windows

Restricted-token sandboxing is enabled by default. If elevated sandbox setup is
not ready, Gateway falls back to restricted-token enforcement. If workspace
write has no enforceable Windows sandbox, the effective policy becomes
read-only. The Agent page displays this effective backend and any fallback
reason.

Restricted Token provides a real filesystem/process-identity boundary. It does
not provide the latest Codex WFP-strength network boundary on its own. The
unelevated compatibility path removes proxy credentials and installs blocking
proxy environment variables; elevated setup can additionally use the offline
identity/firewall path. Domain-scoped Agent network intent is preserved in the
policy fingerprint, but Savfox does not claim to enforce an allow-by-domain
policy until a managed proxy is available.

`granular_approval` currently enforces only `sandbox_approval` and `rules`.
Scoped `RequestPermissions`, Guardian auto-review, and managed domain grants are
deliberately not exposed as configuration until their runtime enforcement paths
are complete.
