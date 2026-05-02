# Codex Upstream Functional Sync Todos

## Scope

Use recent `E:\Repos\codex` `origin/main` commits as references, but implement Savfox-native equivalents instead of direct cherry-picks. Current Codex local `main` is behind `origin/main` by 1551 commits, and Savfox has no common Git ancestor with that repository.

## Useful Features To Implement

### 1. Project-local config safety

- Status: done
- Reference: `9ddb267e9c fix: ignore dangerous project-level config keys (#20098)`
- Tasks:
  - Strip credential-routing and command-execution keys from trusted project `.savfox/config.toml` layers.
  - Keep safe project-local keys working.
  - Preserve startup warnings in `ConfigLayerStack`.
  - Surface warnings through TUI startup history and app-server config warnings.
  - Add config-layer and `ConfigBuilder` regression tests.

### 2. ConfigBuilder stack usage

- Status: done
- Reference: `2817866a32 fix: reduce ConfigBuilder::build stack usage (#20650)`
- Tasks:
  - Keep public `ConfigBuilder::build` as a thin async wrapper.
  - Move the existing implementation into `build_inner`.
  - Box the large config-loading future before awaiting it.

### 3. TUI markdown list spacing after code blocks

- Status: done
- Reference: `52c06b8759 Preserve TUI markdown list spacing after code blocks (#19706)`
- Tasks:
  - Track whether active list items contain code blocks.
  - Add a blank separator before the next list item only when needed.
  - Add focused markdown renderer regression tests.

### 4. Custom CA support for shared outbound HTTP clients

- Status: done
- Reference: `9e905528bb Fix custom CA login behind TLS-inspecting proxies (#20676)`
- Why useful:
  - Enterprise TLS-inspecting proxies can break login, token refresh, model list, update checks, and plugin/network calls unless Savfox consistently honors a custom root CA.
- Tasks:
  - Add a Savfox custom-CA helper that reads `SAVFOX_CA_CERTIFICATE`, falls back to `CODEX_CA_CERTIFICATE`, then `SSL_CERT_FILE`.
  - Parse PEM bundles, including OpenSSL `TRUSTED CERTIFICATE` labels and ignorable CRL blocks.
  - Force reqwest onto rustls before adding custom roots, preserving native roots.
  - Route `crates/core/src/default_client.rs` through the helper.
  - Replace high-value raw `reqwest::Client::new()` / `reqwest::Client::builder().build()` paths that affect login or user-visible network operations.
  - Add unit tests for env precedence and invalid bundle errors.
- Implementation:
  - Added shared custom CA handling in `savfox-http-client`.
  - Wired core default clients, OAuth/device login, skill registry downloads, CLI update downloads, and wizard connection probes through the shared helper.
  - Uses `SAVFOX_CA_CERTIFICATE`, then `CODEX_CA_CERTIFICATE`, then `SSL_CERT_FILE`.

### 5. Bounded TUI startup terminal probes

- Status: done
- Reference: `127434cd8b fix(tui): bound startup terminal probes (#20654)`
- Why useful:
  - Unsupported terminals should not stall TUI startup for seconds while cursor, keyboard, or OSC color probes wait for responses.
- Tasks:
  - Add a small bounded Unix terminal probe module for cursor-position and OSC 10/11 default-color queries.
  - Let `custom_terminal::Terminal` accept a caller-provided startup cursor position.
  - Use the bounded probe during Unix TUI initialization.
  - Keep non-Unix fallback behavior unchanged.
  - Add parser and timeout-path tests that do not require a real terminal.
- Implementation:
  - Added bounded parser/probe module for startup cursor, keyboard enhancement, and OSC color queries.
  - Wired Unix TUI startup to bounded probes; non-Unix keeps existing fallback behavior.
  - Added parser tests that do not require a real terminal.

### 6. Core-produced ImageView item lifecycle

- Status: done
- Reference: `aed74e5ee4 [codex] Emit image view as core item (#20512)`
- Why useful:
  - App-server clients should receive image-view results from the same core item lifecycle as other tool-visible items, instead of reconstructing them from a legacy event.
- Tasks:
  - Inspect current Savfox `ViewImageToolCall` flow and existing `ItemStarted` / `ItemCompleted` conversion.
  - Add or reuse a protocol `TurnItem` / app-server `SessionItem` image-view variant.
  - Emit item start/completion from the core `view_image` handler.
  - Keep legacy `ViewImageToolCall` compatibility for TUI, rollout, and older app-server clients.
  - Add focused core/app-server regression tests.
- Implementation:
  - Added `TurnItem::ImageView`.
  - Core `view_image` now emits item started/completed events and fans out the legacy `ViewImageToolCall` from completion.
  - App-server forwards canonical item lifecycle notifications instead of reconstructing them from the legacy event.

## Useful But Not This Pass

### Multi-environment choices in environment context

- Reference: `2952beb009 Surface multi-environment choices in environment context (#20646)`
- Reason:
  - Useful conceptually, but upstream depends on `context/*`, `environment_selection.rs`, and session/turn-context architecture that does not map cleanly to Savfox's current core layout. Do after a dedicated design pass.

### Plugin marketplace upgrade flow

- Reference: `610eefb86b /plugins: add marketplace upgrade flow (#20478)`
- Reason:
  - Useful only if Savfox wants the same interactive marketplace upgrade UX. Upstream implementation is tied to `chatwidget` and background request plumbing that Savfox does not share.

### Clear live hook rows when turns finalize

- Reference: `d55479488e Clear live hook rows when turns finalize (#20674)`
- Reason:
  - Useful if Savfox has stale live hook rows, but upstream patch is `chatwidget`-specific. Need a Savfox TUI status-row bug reproduction first.

## Not Applicable

- `cd2760fc08` / `466798aa83`: Bazel Windows CI. Savfox does not have a Bazel workspace.
- `a5fbcf1ab4`: code-mode globals. Savfox does not have `crates/code-mode`.
- `ff66b3c7eb`: Alt+Enter newline alias. Savfox's current composer path already lets Alt+Enter fall through as newline.

## Final Verification

- `cargo fmt --all`
- `git diff --check`
- `cargo test --locked -p savfox-http-client custom_ca`
- `cargo test --locked -p savfox-protocol -p savfox-app-server-protocol`
- `cargo test --locked -p savfox-app-server`
- `cargo test --locked -p savfox-core project_layer_ignores_unsupported_config_keys`
- `cargo test --locked -p savfox-core config_builder_ignores_project_local_credential_routing`
- `cargo test --locked -p savfox-core --test all view_image_tool_attaches_local_image` (0 tests on Windows due suite cfg)
- `cargo test --locked -p savfox-tui terminal_probe`
- `cargo test --locked -p savfox-tui markdown_render_tests`
- `cargo test --locked -p savfox-login-oauth`
- `cargo test --locked -p savfox-skill-registry`
- `cargo test --locked -p savfox-cli --lib`
