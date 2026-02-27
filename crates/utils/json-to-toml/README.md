# savfox-utils-json-to-toml

Provides a single function, `json_to_toml`, that converts a `serde_json::Value` into a semantically equivalent `toml::Value`. The mapping is straightforward: booleans, strings, integers, and floats map directly to their TOML counterparts; arrays and objects recurse; JSON `null` is represented as an empty string (since TOML has no null type); and numbers that cannot be represented as `i64` or `f64` fall back to their string representation.

This crate is used within the workspace to bridge JSON-based configuration or protocol data into TOML format where needed.
