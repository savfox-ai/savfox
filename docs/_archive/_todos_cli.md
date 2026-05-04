# Terminal CLI Agent Delegation Todos

## Goal

Add a per-agent terminal CLI backend so an agent can delegate its actual work to local tools such as `codex`, `claude`, or any compatible command, then return the CLI output through existing gateway/chat/session paths.

## Tasks

- [x] Confirm current agent invocation path and whether terminal CLI delegation already exists.
- [x] Define agent config shape for terminal CLI delegation.
- [x] Implement backend resolution and execution logic in the gateway agent invocation path.
- [x] Preserve normal model-backed agent behavior when terminal delegation is disabled or absent.
- [x] Expose terminal delegation fields in shared agent API types.
- [x] Update the agents design UI to configure native model vs terminal CLI delegation.
- [x] Include terminal CLI settings in create, edit, clone, import, export, save, and dirty tracking flows.
- [x] Add focused tests for config normalization/template rendering/CLI output handling where practical.
- [x] Run targeted formatting and tests for the touched crates.
