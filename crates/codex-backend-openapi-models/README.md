# savfox-backend-openapi-models

Auto-generated Rust types corresponding to the backend OpenAPI specification. The `src/models/` directory and its `mod.rs` are populated by a code generation script and should not be edited by hand.

This crate re-exports all generated model structs and enums via `pub mod models`. It uses `serde` and `serde_json` for serialization and `serde_with` for custom (de)serialization behaviors. Workspace lint overrides are applied at the crate level to accommodate generated code that may use `unwrap`/`expect`.
