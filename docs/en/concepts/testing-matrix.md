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
- `tooling-tests`: installer fixtures, Web bundle integrity and targeted package mapping.
- `clippy`: Ubuntu workspace lint checks with `--all-targets --all-features`, including optional integrations.
- `audit`: `cargo audit --deny warnings` rejects new advisory warnings as well as vulnerabilities. Narrow, reviewed maintenance exceptions remain in `.cargo/audit.toml`.
- `test-targeted`: Ubuntu targeted domain checks driven by path filters. Gateway/shared/channel tests run with both default features and `--all-features`, including Arkret. The optional-feature run uses the same Gateway/channel/Web/workspace change conditions as the default run.
- `test`: Ubuntu full workspace nextest plus doctests for workspace-wide changes, CI workflow changes, crates outside the targeted domains, manual runs, and scheduled runs. New crates fall back to this layer until explicitly assigned to a targeted domain.
- `test-cross-platform`: Windows and macOS full workspace nextest, manual or scheduled only.

Regular PRs do not run the full Windows/macOS matrix unless the workflow is manually dispatched.

## Domain mapping

| Domain | Primary command | Scope |
| ------ | --------------- | ----- |
| Core/runtime | `just test-core` | `savfox-core`, config/model/http/api runtime crates; CI also runs `savfox-common` |
| Protocol/editor | `just test-protocol` | `savfox-protocol`, `savfox-app-server-protocol`, `savfox-app-server`, `savfox-mcp-server` |
| TUI | `just test-tui` | `savfox-tui`; CI also runs `savfox-ansi-escape` |
| Gateway | `just test-gateway` | `savfox-gateway-server`, `savfox-gateway-shared`, `savfox-channels` |
| Channels only | `just test-channels` | `savfox-channels` |
| Web | `just test-web` | Glory frontend build |

## Selection rule

`node scripts/check-ci-test-matrix.cjs` compares the workspace package metadata
with CI's workspace exclusions and targeted test commands. Every excluded crate
must have its own explicit targeted test package; building it as a dependency
does not run its unit tests. Web native unit tests run alongside the WASM build.
The checker also requires an audit that denies warnings, all-feature workspace Clippy and explicit all-feature
tests for each Gateway/shared/channel package exposing the `arkret` feature.
These optional checks run on Ubuntu; target-specific Windows keystore
dependencies remain guarded by their Cargo target conditions. This supplements
the default-feature suite rather than replacing it or the cross-platform runs.

Gateway regression fixtures exercise the production router, dispatcher, WebSocket
handshake, configuration validator and persistent session store. They fail when
the expected behavior fails. Standalone JSON construction examples are not
integration coverage. Explicitly running ignored live Gateway E2E tests requires
both `E2E_GATEWAY_URL` and `E2E_GATEWAY_TOKEN`; missing setup fails the test.

Pick the smallest slice that fully covers the changed behavior. If the change crosses a shared boundary, run more than one slice.

## RPC latency baseline

Run `bash scripts/bench-rpc.sh` against its isolated local gateway. It measures
200 sequential `status` round trips after 10 warm-up requests, without invoking a
model. Set `SAVFOX_RPC_BENCH_SAMPLES` to change the positive sample count.
The JSON report includes throughput and p50/p95/p99/max latency and is saved to
`$SAVFOX_HOME/rpc-benchmark.json` (or `.e2e-savfox-home/rpc-benchmark.json`), unless
`SAVFOX_RPC_BENCH_OUTPUT` selects another existing directory. Missing setup,
missing benchmark tests and zero samples fail the run. Use the same build profile
and machine when comparing reports; this is a latency baseline, not a load test.

## Review expectation

PRs should list the chosen test slice, even when the contributor intentionally skips execution and leaves follow-up validation to CI.
