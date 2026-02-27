# savfox-app-server-protocol

Defines the JSON-RPC-based protocol used between the Savfox app server and its clients. This crate contains the full set of request/response/notification types for both v1 and v2 of the protocol, along with common shared types such as thread history structures.

The crate supports schema export in multiple formats: JSON Schema and TypeScript type definitions. These are generated via `generate_json()`, `generate_ts()`, and related functions, and the exported schemas are stored under `schema/json/` and `schema/typescript/`. The `ExperimentalApi` derive macro (from `savfox-experimental-api-macros`) is used to annotate fields that are part of the experimental API surface.

Protocol types use `schemars` for JSON Schema derivation, `ts-rs` for TypeScript generation, and `serde` for serialization. The crate also provides utilities for reading and writing schema fixture files for testing protocol compatibility.
