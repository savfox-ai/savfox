# Local Claude Code through Rust

The experimental `claude_stream` terminal mode drives the local Claude Code CLI over bidirectional JSONL. Savfox starts and owns the process, bridges permissions and questions, and resumes an explicit Claude session on later chat turns. No Python or TypeScript bridge is required. Claude supplies its own model requests, authentication and tools.

Existing `one_shot`, `managed_pty` and Codex `app_server` configurations retain their explicit modes. There is no automatic fallback or prompt replay after an error.

## Configure an agent

First sign in with your local Claude CLI and inspect `claude --version`. In the Agents page, select Claude and **Claude Stream (Rust)**. Fill `expected_version` in the Claude settings with the exact version output after validating that installation. A missing or changed version blocks execution. Version matching and initialization are compatibility gates, not evidence of a live integration test.

An agent file under `<savfox_home>/agents/claude-local.json` can contain:

```json
{
  "id": "claude-local",
  "name": "Local Claude",
  "kind": "terminal",
  "system_prompt": "Follow the repository instructions and explain your changes.",
  "terminal": {
    "runtime": "claude",
    "profile": "claude",
    "enabled": true,
    "command": "claude",
    "mode": "claude_stream",
    "session_scope": "per_session",
    "savfox_approval_bridge": "required",
    "terminal_execution": "native_terminal",
    "timeout_secs": 300,
    "workspace": { "mode": "shared", "cleanup_policy": "per_session" },
    "claude": {
      "expected_version": "REPLACE WITH EXACT claude --version OUTPUT",
      "model": null,
      "settings_policy": "controlled",
      "allowed_tools": [],
      "initialize_timeout_secs": 20,
      "approval_timeout_secs": 120,
      "shutdown_timeout_secs": 5
    }
  }
}
```

Leave `model` unset to use the CLI default; a Savfox provider/model slug is never implicitly forwarded. Set a Claude model name explicitly to select a model. `args` must be empty and `stdin` must be absent. The adapter owns its protocol flags. `io_protocol` may be absent or `jsonl`.

`settings_policy = "controlled"` disables settings sources and hooks and uses strict MCP configuration. Its default MCP configuration has no servers. Optional `mcp_config` is a JSON string with a `mcpServers` object. `user` loads the CLI's normal settings. Neither policy provides a filesystem or network sandbox, nor guarantees that project memory, skills or all extensions are disabled. `allowed_tools` are CLI auto-approval rules, not a tool-visibility allowlist; those calls may bypass the approval bridge.

Claude reads its own local login. Savfox does not copy credentials or inject a provider API key. Explicit `terminal.env` overrides can set credentials, proxy variables or a configuration directory. Directory and effective environment identities enter a digest, without storing key values in session bindings. Switching accounts inside an unchanged login directory cannot currently be detected.

## Chat, approvals and stopping

Use `chat.send` with a client `request_id` and UUID v7 `session_id`. Native CLI execution requires `OperatorWrite`; the default `required` bridge also needs an owner client with `OperatorApprovals` (an `Operator` token supplies both). Events, pending approvals, run queries and Claude history are credential-bound. Tokens with identical display labels are still different identities.

The Approvals page supports **Approve once**, **Deny**, **Stop task**, and `AskUserQuestion` answers. The CLI's original input stays on the server; the browser cannot replace tool parameters. Questions use indexed answer keys in `exec.approval.resolve`, for example `"answers": {"0": "Selected option"}`. The adapter maps these indices to the original question text. Resolutions require the original `request_id`, nonce and owner credential. Expired, canceled and restarted approvals cannot authorize another execution. Recording a decision does not prove tool execution or CLI receipt. MCP elicitation is explicitly unsupported.

`prompt` bridge mode denies permission requests when there is no authorized client. `disabled` is rejected. Channel invocations require a correlated authenticated peer scope; only `prompt` mode can operate without a WS approval client, with permissions denied and trusted per-turn context refreshed separately. There is no Channel approval notification/response bridge for Claude yet.

The chat Stop button calls `chat.abort` with the exact session; API clients can target `run_id`. An unknown explicit target returns not found and never stops unrelated Core sessions. Global Core stopping requires `all=true` and `OperatorAdmin`. Gateway shutdown cancels its Claude runs. Browser detach alone does not cancel accepted work.

Query a retained run with:

```json
{"method":"chat.run.get","params":{"request_id":"your-original-request-id"}}
```

Reconnect using the same credential. The result contains status, reply, diagnostic stderr, vendor usage and any active approval IDs/nonces. Lost text deltas can be calibrated from the final RPC reply or this query; frontend streaming queues are bounded. Usage categories are preserved; missing categories do not produce a fabricated `TokenUsage`. Cache creation/read are included in compatible input totals, with cache-read also reported as its subset. Repeated submissions do not issue a second prompt, write a second rollout or add usage again.

After a gateway restart, unfinished startup/running records report `unknown_after_restart`. Reusing the recorded request ID is rejected; inspect the prior effects before intentionally continuing with a new request ID. Queries are keyed by credential and client request ID.

## Resume and execution limits

`per_session` retains an explicit Claude transcript ID. `per_turn` starts independently and disables transcript persistence. `per_agent` and `manual` are rejected. Restoring a session sends only the current request and attachments, while the first turn imports up to 20 Savfox history messages and uses the system instruction argument. Changed identity, cwd, executable/version, settings, environment, model configuration or initial instructions require a new chat. Missing Claude transcripts return an error and are not automatically recreated.

Resuming a session requires retained workspace/session directories. Per-turn and delete-on-success/always cleanup combinations are rejected. Protocol failures, cancellations and uncertain cleanup retain the workspace. Finite local agent/workflow tasks are tracked after a turn result; indefinite background shells are not treated as finite work. Execution has an overall bounded deadline including one approval-timeout allowance; approval waits have their own expiry and do not extend the total indefinitely.

The current process-group implementation is Unix-only. Windows `claude_stream` startup is explicitly rejected pending Job Object/shim validation. Unix groups do not contain descendants that escape the group. Worktrees and permission prompts are not a sandbox.

Automated tests use a synthetic shell protocol peer, covering streams, usage, explicit resume, permission decisions, questions, cancellation, frame bounds, stderr draining, background continuation, request deduplication and WS credential ownership/reconnect. **No real Claude CLI version is certified by these tests.** Validate local login, real permissions, resume, background behavior and process cleanup before deployment.

The new mode rejects `read_only` and unknown workspace modes; `shared`, `worktree` and `patch_only` describe working directories and patch handling, not OS access restrictions.
