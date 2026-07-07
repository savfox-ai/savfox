# Testing Matrix

Savfox is too large for “always run the whole workspace first” to be the only workflow.

## Layers

### Fast local slice

Use targeted commands during development:
- `just test-core`
- `just test-protocol`
- `just test-tui`
- `just test-gateway`
- `just test-channels`
- `just test-web`

### Full workspace validation

Use `just test` when:
- touching shared behavior with wide blast radius
- preparing release work
- validating cross-crate changes after targeted slices pass

### CI layers

CI now has these validation layers:
- `fmt`: nightly rustfmt, always run.
- `test-targeted`: Ubuntu targeted domain checks driven by path filters.
- `test`: Ubuntu full workspace nextest plus doctests for workspace-wide changes, manual runs, and scheduled runs.
- `test-cross-platform`: Windows and macOS full workspace nextest, manual or scheduled only.

Regular PRs do not run the full Windows/macOS matrix unless the workflow is manually dispatched.

## Domain mapping

| Domain | Primary command | Scope |
| ------ | --------------- | ----- |
| Core/runtime | `just test-core` | `savfox-core`, config/model/http/api runtime crates |
| Protocol/editor | `just test-protocol` | `savfox-protocol`, `savfox-app-server-protocol`, `savfox-app-server`, `savfox-mcp-server` |
| TUI | `just test-tui` | `savfox-tui` |
| Gateway | `just test-gateway` | `savfox-gateway-server`, `savfox-gateway-shared`, `savfox-channels` |
| Channels only | `just test-channels` | `savfox-channels` |
| Web | `just test-web` | Dioxus frontend build |

## Selection rule

Pick the smallest slice that fully covers the changed behavior. If the change crosses a shared boundary, run more than one slice.

## Review expectation

PRs should list the chosen test slice, even when the contributor intentionally skips execution and leaves follow-up validation to CI.
