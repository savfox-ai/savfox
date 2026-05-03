# Crate Boundaries

This document defines where new code should live.

## Boundary rules

### `savfox-core`

Owns transport-independent agent behavior:
- config loading and editing
- auth, sessions, rollout, tools, sandboxing
- model/provider runtime coordination
- skills, memory, prompt assembly

Do not put gateway-only HTTP handlers, Dioxus UI state, or editor-specific JSON-RPC framing here.

### Surface crates

Surface crates should stay thin:
- `savfox-cli`
- `savfox-tui`
- `savfox-app-server`
- `savfox-mcp-server`
- `savfox-gateway-server`
- `savfox-gateway-dioxus`

They may adapt transport, lifecycle, and UX. They should not duplicate core policy or business rules.

### Protocol crates

- `savfox-protocol`: shared protocol and data-model layer across native surfaces.
- `savfox-app-server-protocol`: app-server specific wire contract and export surface.
- `savfox-gateway-shared`: browser/backend shared serde types for gateway UI and RPC.

Protocol crates should carry data contracts, not runtime business logic.

### Integration and support crates

- `savfox-channels`: external messaging platform adapters.
- `savfox-browser-automation`: browser automation runtime.
- `savfox-http-client` / `savfox-api-client`: outbound HTTP and model-provider transport.
- platform sandbox crates: OS-specific execution boundaries.

## Dependency direction

Preferred direction:

```text
surface crates -> core / protocol / support crates
support crates -> protocol / lower-level support crates
core -> protocol / lower-level support crates
```

Avoid these edges:
- `savfox-core` depending on gateway, TUI, app-server, or Dioxus crates
- protocol crates depending on surface crates
- `savfox-gateway-dioxus` depending on native-only runtime crates
- `savfox-channels` depending on TUI or app-server crates

## Placement guide

Add code to `savfox-core` when:
- behavior must be identical across multiple surfaces
- the logic is policy, orchestration, or shared lifecycle

Add code to a surface crate when:
- it is transport framing, UI state, RPC routing, or lifecycle glue
- only one product surface uses it

Add code to a protocol crate when:
- it is a serializable contract crossing a process, machine, or wasm/native boundary

## Review checklist

Before adding a new module, answer:
1. does this need to be identical across surfaces?
2. is this a wire contract or runtime behavior?
3. does the dependency point inward toward shared crates, or outward toward a surface?
4. would placing it here make another crate depend on a higher layer?
