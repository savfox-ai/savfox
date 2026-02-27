# savfox-experimental-api-macros

A proc-macro crate that provides the `#[derive(ExperimentalApi)]` macro. This derive macro is used throughout the `savfox-app-server-protocol` crate to mark specific struct fields or enum variants as part of the experimental API surface.

Fields annotated with `#[experimental("reason")]` are registered via `inventory::submit!` and get an `experimental_reason(&self) -> Option<&'static str>` method generated on the parent type. The macro handles `Option`, `Vec`, `HashMap`, and `bool` fields intelligently -- it only reports a field as "experimental" when it is actually populated (e.g., `Some`, non-empty, or `true`). Field names are converted from `snake_case` to `camelCase` for the serialized schema representation.

For enums, each variant can be individually annotated, and the generated `experimental_reason` method returns the reason string when the active variant is experimental, or `None` otherwise.
