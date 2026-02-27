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

Status can be queried per-thread through the gateway bridge.

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

When a chat message arrives through a bridge (Discord, Telegram, etc.), the
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
