# Multi-Agent System

Savfox supports a multi-agent architecture where a primary agent can delegate
subtasks to specialized child agents. Each agent runs in its own thread with
independent configuration, context, and (optionally) workspace isolation.

## Agent roles

Agent roles are defined in `crates/core/src/agent/role.rs`. Each role carries a
hard-coded `AgentProfile` that overrides specific configuration fields.

| Role           | Description                                                 |
|----------------|-------------------------------------------------------------|
| `default`      | Inherits the parent agent's configuration unchanged         |
| `explorer`     | Fast codebase-question agent with a specific model override |
| `worker`       | Task-executing agent for implementation and fixes           |
| `orchestrator` | Coordination-only agent that delegates to workers (planned) |

### AgentProfile fields

| Field              | Type                    | Description                              |
|--------------------|-------------------------|------------------------------------------|
| `base_instructions`| `Option<&'static str>`  | Override system prompt                   |
| `model`            | `Option<&'static str>`  | Override model ID                        |
| `reasoning_effort` | `Option<ReasoningEffort>`| Override reasoning effort (Low/Medium/High)|
| `read_only`        | `bool`                  | Force read-only sandbox policy           |
| `description`      | `&'static str`          | Description shown in tool specs          |

### Explorer role

The `Explorer` role is optimized for codebase questions:

- Uses a fast model override for quick responses.
- Sets reasoning effort to `Medium`.
- Designed to be run in parallel for independent questions.
- Results should be trusted without re-verification.

### Worker role

The `Worker` role is designed for execution tasks:

- Implements features, fixes bugs, splits refactors.
- Each worker is assigned explicit ownership of specific files.
- Workers are aware they share the codebase with other agents.

## Agent spawning

Sub-agents are spawned via the `ThreadManager`. The spawning process:

1. The parent agent requests a new thread with a specific role.
2. `AgentRole::apply_to_config()` modifies the child's `Config`:
   - Overrides the model if the role specifies one.
   - Sets base instructions from the role profile.
   - Adjusts reasoning effort.
   - Enforces read-only sandbox if required.
3. The child thread starts with its own event loop and context.

### Spawn depth limits

To prevent runaway recursion, agent spawning has a maximum depth limit defined
by `MAX_THREAD_SPAWN_DEPTH` in `crates/core/src/agent/guards.rs`. The current
depth is tracked and checked before each spawn.

## Agent status

Agent status is tracked via `AgentStatus` (from `savfox-protocol`). The status
transitions map the lifecycle of each turn:

```
Idle --> Thinking --> Executing --> Idle
                 \--> Error
```

Status can be queried per-thread through the gateway channel.

## Agent control

`AgentControl` (`crates/core/src/agent/control.rs`) provides the control
interface for each agent thread:

- `submit(Op)` -- submit an operation (user input, interrupt, shutdown).
- `agent_status()` -- query the current status.
- `next_event()` -- receive the next event from the agent.

### Operations (Op)

| Operation         | Description                                    |
|-------------------|------------------------------------------------|
| `UserInput`       | Submit user text and optional attachments       |
| `Interrupt`       | Cancel the current turn                        |
| `Shutdown`        | Gracefully stop the thread                     |
| `SetThreadName`   | Rename the thread                              |
| `ThreadRollback`  | Roll back N turns                              |
| `Review`          | Start a code review                            |

## Delegation

The gateway WS-RPC exposes delegation management methods:

| Method                      | Description                           |
|-----------------------------|---------------------------------------|
| `agent.delegation.list`     | List active delegations               |
| `agent.delegation.chain`    | Get the delegation chain for a thread |
| `agent.delegation.record`   | Record a new delegation               |
| `agent.delegation.remove`   | Remove a delegation record            |

## Terminal Agent Runtime

Gateway agents can also delegate a turn to a local terminal CLI through
`terminal_delegate`. This is a first-class runtime path for tools such as
Codex, Claude, or custom local agents whose native CLI carries its own login,
quota, context, plugins, and interactive behavior.

The current terminal runtime supports the existing one-shot flow: Savfox starts
the configured command, renders the prompt into arguments or stdin, captures
stdout/stderr, stores the exchange in the session rollout, and returns the
captured reply. Interactive launch through `agent.terminal.launch` remains the
operator-controlled path for logging in, using a TUI, or taking over a complex
agent session directly.

The one-shot path runs through the Terminal Supervisor. The supervisor validates
the resolved cwd, spawns the process, writes stdin, captures stdout/stderr with a
bounded reader, kills timed-out processes, and normalizes `spawn`, `invalid cwd`,
`timeout`, `non-zero exit`, and output-read failures into terminal metadata and
user-facing errors. stdout and stderr are capped at 1 MiB per stream; truncated
streams include a truncation marker in the stored log and returned metadata.

Terminal agents execute native local commands. Savfox can record process
metadata, logs, health results, and stream events, but it does not mediate the
vendor CLI's own approvals, plugin actions, file writes, or network calls once
the command is running. Use this runtime for trusted CLIs and choose the cwd
deliberately. The permission and workspace fields below make that boundary
explicit in config, but the current one-shot runtime still starts a native
terminal process. The one-shot backend now enforces `shared`, `worktree`, and
`patch_only` workspace modes; `read_only` is accepted as a declaration but the
runtime reports a capability gap until a platform sandbox is wired in.
Workspace isolation reduces accidental repo collisions, but it is not a
replacement for the native terminal permission boundary by itself.

`terminal_delegate` now accepts optional forward-compatible runtime fields:

| Field                    | Purpose                                      |
|--------------------------|----------------------------------------------|
| `profile`                | Terminal agent profile, such as `codex`, `claude`, or `custom` |
| `mode`                   | Runtime mode, such as `one_shot`, `managed_pty`, or `interactive_launch` |
| `session_scope`          | Whether terminal state is scoped per turn, per session, per agent, or manually |
| `io_protocol`            | How output is interpreted: plain text, JSONL, profile parser, or sentinel |
| `health_check_command` / `health_check_args` | Optional command/args used only by `agent.terminal.health` |
| `terminal_execution`     | Permission boundary declaration for the command: `native_terminal`, `managed_workspace`, or `disabled` |
| `savfox_approval_bridge` | Whether Savfox should bridge vendor CLI approvals: `disabled`, `prompt`, or `required` |
| `workspace`              | Workspace declaration with `mode`, `base`, and `cleanup_policy` |

When these fields are omitted, the runtime treats the agent as
`profile = "custom"`, `mode = "one_shot"`, `session_scope = "per_session"`,
`io_protocol = "plain_text"`, `terminal_execution = "native_terminal"`,
`savfox_approval_bridge = "disabled"`, and `workspace.mode = "shared"`.
`workspace.base` is optional and defaults to the resolved terminal cwd; the
default `workspace.cleanup_policy` is `per_session`. Older `terminal_delegate`
configs with only `enabled`, `command`, `args`, `stdin`, `cwd`, `env`, and
`timeout_secs` continue to work without migration. Unknown string values are
preserved by clients and the shared wire types so newer runtimes can introduce
additional policies without breaking older UIs.

The `workspace` block is:

| Field            | Values / meaning |
|------------------|------------------|
| `mode`           | `shared` uses the configured cwd directly; `worktree` creates a detached git worktree per terminal session; `patch_only` runs in an isolated worktree and writes `workspace.patch` plus `workspace-diff-summary.txt` under `log_dir`; `read_only` currently returns a clear unsupported-capability error |
| `base`           | Optional base path or template used when a managed workspace is created |
| `cleanup_policy` | `per_session`/`manual` preserve the isolated workspace; `per_turn`/`delete_on_success` removes it after a successful run; `delete_always` also removes it after failures |

For `worktree` and `patch_only`, recovery starts from `metadata.json`: inspect
`workspace_path`, `workspace_cleanup_status`, `workspace_patch_path`, and
`workspace_diff_summary_path`. If cleanup fails, remove the detached worktree
manually with `git worktree remove --force <workspace_path>` from the recorded
`workspace_base` git root. If `patch_only` generated a patch, review
`workspace-diff-summary.txt` before applying `workspace.patch` to the main
working tree.

`savfox_approval_bridge = "disabled"` means the vendor CLI keeps its own
approval prompts and Savfox does not inspect each tool/file/network action
inside that CLI. `prompt` and `required` are forward-compatible declarations for
future bridge implementations; do not rely on them as a sandbox until the
runtime explicitly reports bridge support. Likewise, `terminal_execution` is a
policy declaration in this compatibility slice; use `enabled = false` to disable
terminal delegation in runtimes that have not implemented policy enforcement.

For each terminal invocation, Savfox creates a session-scoped directory under
`{savfox_home}/terminal-agents/<agent_id>/sessions/<session_id>/` with `home`,
`workspace`, `logs`, and `metadata.json` entries. Terminal command templates can
reference these values with:

| Template              | Value                                  |
|-----------------------|----------------------------------------|
| `{{session_id}}`      | The Savfox session id for this agent turn |
| `{{agent_home}}`      | Isolated home/config directory for the terminal agent |
| `{{workspace_dir}}`   | Session workspace directory reserved for terminal runtime use |
| `{{log_dir}}`         | Directory where terminal stdout/stderr logs are written |

`metadata.json` records the terminal profile, mode, session scope, I/O protocol,
resolved paths, command, cwd, workspace mode/base/path, cleanup status, patch
path, diff summary path, process id, status, start/completion timestamps, exit
code, errors, and the Savfox rollout path when one is persisted. Session ids
passed into the terminal runtime must be UUID v7 values.

During WS-RPC session turns, terminal agents emit the same user-facing stream
topics as model agents plus terminal-specific markers: `started`, `log`,
`status`, `message`, `completed`, and `error`. The complete payload also carries
the parsed terminal event list so clients can build a more detailed timeline.
The Sessions UI uses that payload for terminal-agent turns instead of dropping
the one-shot event stream on the floor.

For output parsing, `io_protocol = "plain_text"` treats stdout as the reply and
stderr as logs. `io_protocol = "jsonl"` accepts line-delimited events with
`event`/`type`/`kind` values such as `message`, `status`, `log`, `error`, and
`completed`; unknown JSONL lines are preserved as raw stdout logs.
`io_protocol = "sentinel"` recognizes lines such as `::savfox-message ...`,
`::savfox-status ...`, `::savfox-error ...`, and `::savfox-complete`.
`io_protocol = "profile"` currently falls back to the plain-text parser unless
a later profile implementation supplies a more specific parser.

Operators can run `agent.terminal.health` to check whether the configured CLI is
available, whether its version command works, and whether the current cwd and
terminal runtime root are usable. Agent configs can override the health probe
with `health_check_command` and `health_check_args`; explicit RPC parameters
still take precedence. `agent.terminal.metrics` returns runtime counters such as
spawn count, duration, timeout count, and exit-reason totals.
`agent.terminal.cleanup` removes terminal session directories under
`{savfox_home}/terminal-agents` and supports `dry_run` for inspection. The
Agents UI exposes health checks next to the interactive launch action. The
create/edit UI also exposes first-level templates for a normal Savfox model
agent, Codex, Claude, and a custom CLI.

Managed PTY sessions are available through WS-RPC for clients that need a
long-lived terminal process: `agent.terminal.pty.start`, `write`, `read`,
`resize`, `close`, `list`, and `close_idle`. Start accepts an explicit command
or resolves the configured `terminal_delegate.interactive_command` / `command`;
write supports text, line, newline, interrupt, control sequence, and manual
completion messages; read can return the transcript since a sequence number or
wait for text with a timeout. Mutating PTY methods require Admin scope, while
`read` and `list` require Read scope.

Example Codex one-shot terminal agent:

```json
{
  "terminal_delegate": {
    "enabled": true,
    "profile": "codex",
    "mode": "one_shot",
    "session_scope": "per_session",
    "io_protocol": "plain_text",
    "terminal_execution": "native_terminal",
    "savfox_approval_bridge": "disabled",
    "workspace": {
      "mode": "shared",
      "cleanup_policy": "per_session"
    },
    "command": "codex",
    "args": ["exec", "{{prompt}}"],
    "interactive_command": "codex"
  }
}
```

Example Claude one-shot terminal agent:

```json
{
  "terminal_delegate": {
    "enabled": true,
    "profile": "claude",
    "mode": "one_shot",
    "session_scope": "per_session",
    "io_protocol": "plain_text",
    "terminal_execution": "native_terminal",
    "savfox_approval_bridge": "disabled",
    "workspace": {
      "mode": "shared",
      "cleanup_policy": "per_session"
    },
    "command": "claude",
    "args": ["-p", "{{prompt}}"],
    "interactive_command": "claude"
  }
}
```

These paths establish the context boundary used by one-shot and managed PTY
terminal sessions while preserving the current one-shot command behavior.

### Managed PTY platform status

Managed PTY is implemented as a gateway-managed session registry with a
process-backed backend. It supports public WS-RPC start/write/read/resize/close
operations, transcript reads, resize/kill trait calls, idle and explicit close,
and sentinel/manual completion for fake REPL tests. It is not yet a native
terminal emulator.

| Platform | Current backend | Native PTY hook | Status |
|----------|-----------------|-----------------|--------|
| Windows  | process-backed stdio backend | ConPTY hook planned | WS-RPC usable; not native PTY |
| macOS    | process-backed stdio backend | Unix pty hook planned | WS-RPC usable; not native PTY |
| Linux    | process-backed stdio backend | Unix pty hook planned | WS-RPC usable; not native PTY |

Clients should not assume full Codex or Claude interactive protocol support
until native PTY backends and approval bridge support are explicitly reported by
the runtime.

## Gateway agent management

### REST API

| Endpoint                         | Method | Description              |
|----------------------------------|--------|--------------------------|
| `/api/agents`                    | GET    | List configured agents   |
| `/api/agents`                    | POST   | Create a new agent       |
| `/api/agents/<agent_id>`         | GET    | Get agent details        |
| `/api/agents/<agent_id>`         | POST   | Update agent config      |
| `/api/agents/<agent_id>`         | DELETE | Delete an agent          |

### WS-RPC methods

| Method             | Scope | Description                          |
|--------------------|-------|--------------------------------------|
| `agents.list`      | Read  | List all agents                      |
| `agents.create`    | Write | Create a new agent definition        |
| `agents.update`    | Write | Update agent configuration           |
| `agents.delete`    | Write | Delete an agent                      |
| `agents.files.list`| Read  | List agent memory files              |
| `agents.files.get` | Read  | Read an agent file                   |
| `agents.files.set` | Write | Write an agent file                  |
| `agent.terminal.health` | Read | Check terminal delegate command health |
| `agent.terminal.metrics` | Read | Inspect terminal runtime metrics |
| `agent.terminal.cleanup` | Admin | Clean terminal session directories |
| `agent.terminal.launch` | Admin | Open an interactive local terminal |
| `agent.terminal.pty.start` | Admin | Start or attach a managed PTY session |
| `agent.terminal.pty.write` | Admin | Write input/control messages to managed PTY |
| `agent.terminal.pty.read` | Read | Read managed PTY transcript entries |
| `agent.terminal.pty.resize` | Admin | Resize managed PTY session metadata/backend |
| `agent.terminal.pty.close` | Admin | Close a managed PTY session |
| `agent.terminal.pty.list` | Read | List managed PTY session metadata |
| `agent.terminal.pty.close_idle` | Admin | Close idle managed PTY sessions |

## Workspace isolation

Each agent thread operates with its own configuration context. Isolation is
enforced through:

1. **Sandbox policies** -- Each thread inherits or overrides the sandbox
   policy. Roles like `Explorer` can enforce read-only access.
2. **Separate config** -- `AgentRole::apply_to_config()` produces a modified
   `Config` for the child thread, so model, instructions, and sandbox settings
   are independent.
3. **Thread-level state** -- Each thread maintains its own message history,
   rollout file, and event stream.

## Routing

When a chat message arrives through a channel (Discord, Telegram, etc.), the
gateway routes it to the appropriate agent thread using session keys:

1. A session key is built from the agent ID, channel, group, and peer.
2. The `SessionStore` resolves the key to a session entry.
3. If a thread exists for the session, the message is routed there.
4. Otherwise, a new thread is spawned with the configured agent role.

The `DmScope` enum controls how direct-message sessions are keyed:

| DmScope          | Key pattern                          |
|------------------|--------------------------------------|
| `Main`           | `agent:{id}:main`                    |
| `PerPeer`        | `agent:{id}:peer:{peer_id}`          |
| `PerChannelPeer` | `agent:{id}:{channel}:peer:{peer_id}`|

Group sessions always include the group ID: `agent:{id}:{channel}:group:{gid}`.
