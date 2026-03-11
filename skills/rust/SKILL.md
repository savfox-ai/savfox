---
name: rust
description: Build, test, and manage Rust projects with Cargo.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🦀"
    requires:
      bins:
        - cargo
      env: []
    install:
      - id: manual
        kind: manual
        instructions: "Install via rustup: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        bins: [cargo, rustc, rustup]
        label: rustup
---

# Rust Skill

Build, test, and manage Rust projects.

## Build

```bash
cargo build
cargo build --release
```

Check without building:
```bash
cargo check
```

## Test

```bash
cargo test
cargo test test_name
cargo test -- --nocapture   # show stdout
cargo test -p crate-name    # specific crate
```

## Run

```bash
cargo run
cargo run -- arg1 arg2
cargo run --release
```

## Format and Lint

```bash
cargo fmt
cargo fmt -- --check    # CI mode
cargo clippy
cargo clippy -- -D warnings
```

## Dependencies

Add dependency:
```bash
cargo add serde --features derive
cargo add tokio --features full
```

Update dependencies:
```bash
cargo update
```

Check outdated:
```bash
cargo outdated
```

## Documentation

Build docs:
```bash
cargo doc --open
```

## Workspace

Check all crates:
```bash
cargo check --workspace
cargo test --workspace
```

## Useful Tools

```bash
cargo install cargo-watch    # auto-rebuild
cargo install cargo-expand   # macro expansion
cargo install cargo-tarpaulin # code coverage
```

Auto-rebuild on change:
```bash
cargo watch -x check -x test
```

## Guidelines

- Use `cargo check` for fast iteration (no codegen)
- Use `cargo clippy` before committing
- Use workspace dependencies for consistency
- Use `--release` for benchmarks and production
- Use `cargo test -- --test-threads=1` for serial tests
