# Execution Tasks

## Phase 1

- [x] Add a shared gateway home-path helper module for repeated files under `savfox_home`, and migrate the duplicated callers in gateway runtime code.

## Phase 2

- [x] Move skill config path normalization to a shared `savfox_core` helper and update the TUI skills flow to reuse it.

## Phase 3

- [x] Remove the duplicated gateway e2e HTTP client helper and reuse the shared test helper.

## Phase 4

- [x] Run targeted verification for the touched crates/files.
- [x] Mark completed tasks in this file.

## Verification Notes

- `cargo check -p savfox-core --lib` passed.
- `cargo check -p savfox-tui --lib` passed.
- `cargo check -p savfox-gateway-server --lib` passed.
- `cargo check -p savfox-tui --tests` passed.
- `cargo check -p savfox-core --tests` still fails in pre-existing unrelated test/lib-test areas outside this task.
- `cargo check -p savfox-gateway-server --tests` still fails in pre-existing unrelated lib-test areas outside this task.
- `cargo fmt --all -- --check` still reports pre-existing workspace-wide formatting drift outside this task, so only touched files were formatted directly.
