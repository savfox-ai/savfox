# Protocol Ownership

Savfox uses three protocol layers. They are related but not interchangeable.

## Ownership table

| Crate | Owns | Should not own |
| ----- | ---- | -------------- |
| `savfox-protocol` | shared domain models used across native surfaces; tool/user-input/config/session primitives | editor-only JSON-RPC wrappers; browser-only view models |
| `savfox-app-server-protocol` | app-server JSON-RPC requests, notifications, exported TS/schema surface | generic runtime business logic; gateway browser models |
| `savfox-gateway-shared` | gateway web UI and backend shared serde types | TUI/editor-specific contracts; core-only internal models |

## Decision rules

### Put a type in `savfox-protocol` when

- more than one native surface needs the same domain concept
- the type is about shared agent/runtime semantics
- the data should stay transport-neutral

Examples:
- approvals
- user input requests
- shared config enums
- session and content items

### Put a type in `savfox-app-server-protocol` when

- the type exists because the app-server speaks JSON-RPC to IDE/editor clients
- export tooling must include it
- it carries app-server protocol versioning concerns

### Put a type in `savfox-gateway-shared` when

- the browser frontend and gateway backend share it directly
- it is specific to gateway REST/WebSocket or web UI behavior
- wasm/native serde compatibility is the main requirement

## Anti-patterns

Avoid:
- copying the same semantic type into multiple protocol crates
- putting runtime helper methods or service behavior into protocol crates
- moving a generic domain type into a surface-specific crate just because one surface touched it first

## Migration rule

If a type begins in a surface-specific protocol crate and later becomes cross-surface:
1. move the canonical domain type into `savfox-protocol`
2. keep a compatibility wrapper or conversion at the surface edge
3. update schema/export code after the canonical move

## Current review focus

When touching protocol code, explicitly check:
- whether a new type is actually browser-specific or editor-specific
- whether an existing type in another protocol crate already expresses the same concept
- whether the change requires regenerated schema or TypeScript artifacts
