# Duplication Analysis Report

## Scope

This report focuses on the requests in `_todo.md`:

- duplicated save/load logic for data stored under `savfox_home`
- duplicated logic between gateway and TUI
- gateway code that should reuse existing workspace functionality
- duplicated Rust frontend/backend data structures or logic
- TUI improvement suggestions from both engineering and user perspectives
- broader security, performance, style, and structure suggestions

## Findings

### 1. `savfox_home` persistence logic is duplicated in the gateway

The strongest repetition is in `crates/gateway-server`, where several modules build paths under the user home directory independently instead of sharing one path layer.

Concrete duplication found:

- `crates/gateway-server/src/tts_service.rs`
- `crates/gateway-server/src/talk_mode/mod.rs`
- `crates/gateway-server/src/voice_wake/mod.rs`
- `crates/gateway-server/src/stt.rs`
- `crates/gateway-server/src/approval_policy_store.rs`
- `crates/gateway-server/src/ws_rpc/mod.rs`
- `crates/gateway-server/src/ws_rpc/handlers/config.rs`
- `crates/gateway-server/src/ws_rpc/handlers/system.rs`

Typical repeated pattern:

- `savfox_home.join("gateway").join("<file>.json")`
- `savfox_home.join("<file>.json")`
- direct `join("config.toml")`

This is not just cosmetic duplication. It spreads storage layout knowledge across unrelated modules, makes filename changes error-prone, and increases the chance that future gateway features create yet another path convention.

At the same time, other parts of the workspace already centralize similar concerns well:

- `savfox_core::config::provider_store`
- `savfox_core::config::channel_store`
- `savfox_core::rollout::session_index`
- `savfox_core::skills::system`

Conclusion:

- The gateway should have a small shared home-path helper layer.
- Existing code that touches `config.toml` should reuse `savfox_core::config::CONFIG_TOML_FILE` instead of repeating the filename literal.

### 2. There is direct conceptual duplication between core config editing and TUI skill state handling

The clearest gateway/TUI-adjacent cross-crate duplication is the skill-path normalization logic:

- `crates/core/src/config/edit.rs`
- `crates/tui/src/chat_screen/skills.rs`

Both normalize skill paths via canonicalization before comparing or persisting them. They currently do the same job with different return types and separate implementations.

Impact:

- Behavior can drift if one side changes canonicalization rules.
- TUI and config persistence may disagree about whether two skill paths are the same path.
- Symlink and path casing bugs become harder to reason about.

Conclusion:

- The normalization rule should live in `savfox_core`, and TUI should call that shared helper instead of maintaining a copy.

### 3. Some gateway code already reuses workspace logic well

Not all apparent duplication is real duplication. A few important areas are already correctly shared:

- session path lookup and archived session lookup are already reused from core rollout/session helpers
- model listing flows in the gateway already delegate into shared core/model code instead of reimplementing provider discovery
- channel config persistence is already centralized through `savfox_core::config::channel_store`

This matters because it suggests the right direction for cleanup:

- keep gateway transport/UI-specific logic in the gateway
- move only generic persistence/path logic into shared helpers
- do not over-extract protocol-specific behavior that genuinely belongs to gateway handlers

### 4. Frontend/backend sharing is improved, but still incomplete

The workspace already has a solid shared crate for gateway API shapes:

- `crates/gateway-shared`

That is a good pattern and should be extended where duplication still exists.

Observed remaining duplication candidates:

- `crates/app-server-protocol/src/protocol/v1.rs` defines `ToolRequestUserInput*` types
- `crates/protocol/src/request_user_input.rs` defines `RequestUserInput*` types with overlapping intent
- some gateway frontend page-local response structs remain in `crates/gateway-dioxus` instead of moving into `crates/gateway-shared`

Conclusion:

- shared wire types should continue moving toward `gateway-shared` or the existing protocol crates instead of being redefined at the edges
- `request_user_input` shapes are a likely future consolidation target

### 5. Test helpers are also duplicated inside the gateway

There is a smaller but clean duplication in tests:

- `crates/gateway-server/tests/helpers/mod.rs` provides `http_client()`
- `crates/gateway-server/tests/e2e_gateway.rs` defines another `http_client()`

This is low-risk and worth fixing because it removes needless divergence in timeout defaults and test plumbing.

### 6. TUI improvement suggestions

#### Engineering perspective

- Skill list state should rely on shared normalized path helpers from core instead of local copies.
- The TUI currently carries both `skills_all` and derived toggle state bookkeeping. More view-model shaping near the data boundary would simplify updates and make tests easier.
- Repeated formatting helpers across TUI/gateway should be reviewed for a shared presentation layer where formatting semantics are intended to match.
- Symlink-sensitive comparisons should be centralized. TUI is especially exposed because user interactions often start from paths copied from the filesystem.

#### User perspective

- The skills management popup would benefit from clearer dirty-state signaling before close. Right now feedback is mostly aggregate counts after closing.
- Large skill sets need better scanability: search/filtering, scope labels, and dependency hints would make the popup more usable.
- If canonicalization collapses two distinct-looking paths to the same target, the UI should be explicit about that rather than silently toggling one shared normalized entry.
- Persist/apply failures should be surfaced inline per item when possible instead of only through general messages.

### 7. Security, performance, style, and structure suggestions

#### Security

- Several handlers serialize JSON manually and write via `tokio::fs::write`. Where atomic persistence matters, prefer a shared safe write path instead of ad hoc writes.
- Config filename literals should be centralized to avoid accidental writes to the wrong file.
- The prompt injection scanner in `ws_rpc/handlers/system.rs` is intentionally heuristic; its API should remain clearly framed as advisory, not authoritative.

#### Performance

- Log rotation currently buffers all log lines in memory before writing the archive. This is simple and acceptable for modest volumes, but it will scale poorly with large buffers.
- Repeated canonicalization in hot UI paths should be monitored if the skill list grows very large; centralization makes later caching easier if it becomes necessary.

#### Style and structure

- Storage layout knowledge should live in one module per domain.
- Shared wire/data types should be owned by shared crates, not duplicated in leaf crates.
- Repeated literal filenames are a maintainability smell even when the behavior is currently correct.

## Executable Work Chosen From This Report

The following items are safe to implement immediately without broad architectural churn:

1. Centralize gateway `savfox_home` path construction for the duplicated config/store files.
2. Reuse one shared skill-path normalization helper between core config persistence and TUI skill UI logic.
3. Remove duplicated gateway e2e HTTP client helper usage.

These items are converted into `_tasks.md` and executed next.
